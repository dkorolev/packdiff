//! packdiff — pack the diff between two git refs into ONE self-contained HTML
//! review page with a WASM comment engine inlined.
//!
//! CLI contract (house principles):
//! - Human-friendly by default at a terminal; **machine mode** (two-space
//!   JSON, single-key union documents) whenever `--json` is given OR stdout
//!   is not a TTY.
//! - Machine output is single-key documents, success and failure alike:
//!   `{ "Packed": { … } }`, `{ "UnknownRef": { … } }`. The exit code stays
//!   the authoritative success signal; `help exitcodes` prints the table.
//! - Liberal acceptance for humans (non-canonical spellings resolve, with a
//!   dim nudge to the canonical form on stderr); machines get non-canonical
//!   invocations refused with the canonical syntax told back to them.
//! - Data → stdout; diagnostics, warnings, liveness → stderr. Warnings land
//!   in BOTH channels in machine mode.
//! - Ctrl+C exits 130 (default signal death); a broken pipe ends the program
//!   quietly.

use std::io::{IsTerminal, Write};

use packdiff::progress::{Progress, Stage};
use packdiff::{pack, Error as CliError, PackOptions};

const HELP: &str = "\
packdiff — pack a git diff into one self-contained HTML review page

USAGE:
  packdiff <BASE> [HEAD] [OPTIONS]
  packdiff <BASE>...<HEAD> [OPTIONS]     merge-base diff (the default semantics)
  packdiff <BASE>..<HEAD> [OPTIONS]      literal two-dot diff
  packdiff help [exitcodes]              this help, or the exit-code table

ARGS:
  BASE                  base ref: branch, tag, or SHA
  HEAD                  head ref (default: HEAD — whatever is checked out)

OPTIONS:
  -C, --repo <REPO>     path to the git repository (default: .)
  -o, --out <OUT>       output HTML path, or '-' for stdout
                        (default: packdiff-<base>-<head>.html)
      --context <N>     diff context lines (default: 3)
      --no-merge-base   diff BASE..HEAD literally instead of
                        merge-base(BASE, HEAD)..HEAD
      --title <T>       override the page title
      --dump-json <P>   also write the typed DiffDocument as JSON
      --json            machine mode: single-key JSON documents on stdout
                        (auto-enabled whenever stdout is not a terminal)
      --open            open the generated page in the default browser
      --color <MODE>    auto | always | never (default: auto; NO_COLOR honored)
      --verbose         echo git invocations and timing to stderr
  -h, --help            print this help
  -V, --version         print version

OUTPUT MODES:
  A terminal gets a colored human summary. Piped/redirected stdout (or
  --json) gets exactly one two-space-indented JSON document per run:
  { \"Packed\": { ... } } on success, or an error document such as
  { \"UnknownRef\": { \"stage\": \"ref\", ... } }. Exception: `-o -` streams
  the HTML page itself on stdout and prints no document.

EXIT CODES: run `packdiff help exitcodes` for the full table.

NOT THIS TOOL'S JOB:
  posting comments to a code host    -> your forge's UI or API (gh, glab)
  side-by-side / syntax highlighting -> deferred; tracked in the README
  watching a branch for changes      -> your shell loop or CI

EXAMPLES:
  packdiff main                            review the current branch against main
  packdiff main feature -o review.html     explicit refs, explicit output
  packdiff origin/main...HEAD --json       PR-style range, summary for scripts
  packdiff v1.0.0..v1.1.0 -C ~/src/app     literal tag-to-tag diff
  packdiff main --open                     render and open in the browser";

const HELP_EXITCODES: &str = "\
packdiff exit codes:

  CODE  STAGE  MEANING                                     CALLER'S NEXT MOVE
  0     -      success                                     use the output
  2     usage  malformed or non-canonical invocation       fix the command line
  3     repo   --repo is not inside a git work tree        point -C at a repository
  4     ref    BASE or HEAD did not resolve to a commit    check `git branch -a` spelling
  5     git    git failed, hung (watchdog), or I/O error   read the message; often transient
  130   -      interrupted (Ctrl+C)                        rerun when ready

The same classification rides inside every machine-mode error document as
its `stage` field, so scripts can branch without memorizing codes.";

/// Everything one invocation resolved to.
struct Args {
  base: String,
  head: String,
  repo: String,
  out: Option<String>,
  context: u32,
  merge_base: bool,
  title: Option<String>,
  dump_json: Option<String>,
  open: bool,
  color: ColorMode,
  verbose: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum ColorMode {
  Auto,
  Always,
  Never,
}

impl ColorMode {
  fn enabled(self, is_tty: bool) -> bool {
    match self {
      ColorMode::Always => true,
      ColorMode::Never => false,
      ColorMode::Auto => is_tty && std::env::var_os("NO_COLOR").is_none(),
    }
  }
}

/// What the caller asked for, before any git work happens.
enum Invocation {
  Run(Box<Args>),
  Help { topic: Option<String> },
  Version,
}

/// `a...b` → merge-base semantics, `a..b` → literal. Git forbids `..` inside
/// ref names, so splitting on the first occurrence is unambiguous.
fn split_range(spec: &str) -> Option<(String, String, bool)> {
  if let Some((base, head)) = spec.split_once("...") {
    if !base.is_empty() && !head.is_empty() {
      return Some((base.to_string(), head.to_string(), true));
    }
  } else if let Some((base, head)) = spec.split_once("..") {
    if !base.is_empty() && !head.is_empty() {
      return Some((base.to_string(), head.to_string(), false));
    }
  }
  None
}

/// Split `--flag=value` into its parts; both spellings are canonical.
fn split_eq(arg: &str) -> (String, Option<String>) {
  match arg.split_once('=') {
    Some((flag, value)) if flag.starts_with("--") => (flag.to_string(), Some(value.to_string())),
    _ => (arg.to_string(), None),
  }
}

/// Liberal-for-humans spellings and their canonical forms. In machine mode
/// these are refused with the canonical syntax; for humans they resolve with
/// a dim nudge on stderr.
fn non_canonical(flag: &str) -> Option<&'static str> {
  match flag {
    "--no-color" | "--no-colour" | "--colour" => Some("--color=never"),
    "-v" => Some("--verbose"),
    _ => None,
  }
}

fn parse_args(argv: &[String], machine: bool) -> Result<Invocation, CliError> {
  if argv.is_empty() {
    return Ok(Invocation::Help { topic: None });
  }
  // Bare `version` counts as an alias only in command position — a branch
  // named `version` in ref position must stay a ref.
  if argv[0] == "version" && argv.len() == 1 {
    if machine {
      return Err(CliError::NonCanonical { given: "version".to_string(), canonical: "--version".to_string() });
    }
    nudge("version", "--version");
    return Ok(Invocation::Version);
  }

  let mut positional: Vec<String> = Vec::new();
  let mut repo = ".".to_string();
  let mut out = None;
  let mut context = 3u32;
  let mut no_merge_base = false;
  let mut title = None;
  let mut dump_json = None;
  let mut json_flag = false;
  let mut open = false;
  let mut color = ColorMode::Auto;
  let mut verbose = false;

  let usage = |msg: String| CliError::Usage { message: format!("{msg} (see `packdiff help`)") };

  let mut i = 0;
  while i < argv.len() {
    let raw = argv[i].clone();
    i += 1;
    let (mut flag, mut inline_value) = split_eq(&raw);

    if let Some(canonical) = non_canonical(&flag) {
      if machine {
        return Err(CliError::NonCanonical { given: raw, canonical: canonical.to_string() });
      }
      nudge(&raw, canonical);
      let (c_flag, c_value) = split_eq(canonical);
      flag = c_flag;
      inline_value = c_value;
    }

    let mut need = |name: &str| -> Result<String, CliError> {
      if let Some(v) = inline_value.clone() {
        return Ok(v);
      }
      if i < argv.len() {
        let v = argv[i].clone();
        i += 1;
        Ok(v)
      } else {
        Err(usage(format!("{name} requires a value")))
      }
    };

    match flag.as_str() {
      "-h" | "--help" => return Ok(Invocation::Help { topic: None }),
      "help" if positional.is_empty() => {
        let topic = if i < argv.len() { Some(argv[i].clone()) } else { None };
        return Ok(Invocation::Help { topic });
      }
      "-V" | "--version" => return Ok(Invocation::Version),
      "-C" | "--repo" => repo = need(&flag)?,
      "-o" | "--out" => out = Some(need(&flag)?),
      "--context" => {
        context = need(&flag)?.parse().map_err(|_| usage("--context expects a non-negative integer".into()))?
      }
      "--no-merge-base" => no_merge_base = true,
      "--title" => title = Some(need(&flag)?),
      "--dump-json" => dump_json = Some(need(&flag)?),
      "--json" => json_flag = true,
      "--open" => open = true,
      "--verbose" => verbose = true,
      "--color" => {
        color = match need(&flag)?.as_str() {
          "auto" => ColorMode::Auto,
          "always" => ColorMode::Always,
          "never" | "no" => ColorMode::Never,
          other => return Err(usage(format!("--color expects auto|always|never, got {other:?}"))),
        }
      }
      other if other.starts_with('-') && other != "-" => {
        return Err(usage(format!("unknown option: {other}")));
      }
      _ => positional.push(raw),
    }
  }

  let (base, head, range_merge_base) = match positional.len() {
    1 => match split_range(&positional[0]) {
      Some((base, head, merge_base)) => (base, head, Some(merge_base)),
      None => (positional[0].clone(), "HEAD".to_string(), None),
    },
    2 => (positional[0].clone(), positional[1].clone(), None),
    n => {
      return Err(usage(format!("expected <BASE> [HEAD] or <BASE>..[.]<HEAD>, got {n} positional argument(s)")));
    }
  };
  // An explicit `..` range implies literal mode; `--no-merge-base` forces it.
  let merge_base = range_merge_base.unwrap_or(true) && !no_merge_base;

  if json_flag && out.as_deref() == Some("-") {
    return Err(usage("cannot combine --json with `-o -`: both claim stdout".into()));
  }

  Ok(Invocation::Run(Box::new(Args {
    base,
    head,
    repo,
    out,
    context,
    merge_base,
    title,
    dump_json,
    open,
    color,
    verbose,
  })))
}

/// Print the canonical-form recommendation for a resolved non-canonical
/// invocation: dim on a color terminal, plain otherwise. Humans only.
fn nudge(given: &str, canonical: &str) {
  let dim = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
  let (d, r) = if dim { ("\x1b[2m", "\x1b[0m") } else { ("", "") };
  eprintln!("{d}note: `{given}` accepted; the canonical form is `{canonical}`{r}");
}

/// Write to stdout, dying quietly on a broken pipe (`cmd | head` is not an
/// error) and loudly on anything else.
fn write_stdout(bytes: &[u8]) -> Result<(), CliError> {
  match std::io::stdout().write_all(bytes) {
    Ok(()) => Ok(()),
    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => std::process::exit(0),
    Err(e) => Err(CliError::Io { message: format!("failed to write stdout: {e}") }),
  }
}

/// A ref name reduced to filename-safe characters (`origin/main` → `origin-main`).
fn sanitize_ref(name: &str) -> String {
  name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect()
}

fn run(args: &Args, machine: bool) -> Result<(), CliError> {
  let mut warnings: Vec<String> = Vec::new();
  // Dropped on any error path below, which stops reporting WITHOUT a final
  // `Done` — only the success path ends the stream with `Done`.
  let progress = Progress::new(machine);
  let mut opts = PackOptions::new(&*args.repo, &*args.base, &*args.head);
  opts.merge_base = args.merge_base;
  opts.context = args.context;
  opts.title = args.title.clone();
  // pack() drives the Resolve → Render stages; Write stays the CLI's.
  let output = pack(&opts, &progress)?;
  let (doc, page) = (output.document, output.html);

  progress.stage(Stage::Write, 1);
  let out_path = match args.out.as_deref() {
    Some("-") => {
      write_stdout(page.as_bytes())?;
      "-".to_string()
    }
    chosen => {
      let path = chosen
        .map(str::to_string)
        .unwrap_or_else(|| format!("packdiff-{}-{}.html", sanitize_ref(&args.base), sanitize_ref(&args.head)));
      if let Some(parent) = std::path::Path::new(&path).parent() {
        if !parent.as_os_str().is_empty() {
          std::fs::create_dir_all(parent)
            .map_err(|e| CliError::Io { message: format!("cannot create {}: {e}", parent.display()) })?;
        }
      }
      std::fs::write(&path, &page).map_err(|e| CliError::Io { message: format!("cannot write {path}: {e}") })?;
      path
    }
  };
  progress.step(&out_path);

  if let Some(dump) = &args.dump_json {
    let mut json =
      serde_json::to_string_pretty(&doc).expect("DiffDocument serializes: no non-string keys, no fallible types");
    json.push('\n');
    std::fs::write(dump, json).map_err(|e| CliError::Io { message: format!("cannot write {dump}: {e}") })?;
  }
  progress.finish();

  if args.open {
    if out_path == "-" {
      warnings.push("--open ignored: the page went to stdout, there is no file to open".to_string());
    } else {
      let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
      if let Err(e) = std::process::Command::new(opener).arg(&out_path).spawn() {
        warnings.push(format!("could not launch {opener}: {e}"));
      }
    }
  }

  // Warnings land in BOTH channels: stderr for humans, and (below) inside
  // the machine document.
  for w in &warnings {
    eprintln!("warning: {w}");
  }

  if machine {
    if out_path != "-" {
      let document = serde_json::json!({
        "Packed": {
          "out": out_path,
          "repo": doc.repo,
          "base": doc.base,
          "head": doc.head,
          "merge_base": doc.merge_base,
          "commits": doc.commits.len(),
          "files": doc.files.len(),
          "additions": doc.additions(),
          "deletions": doc.deletions(),
          "binary_files": doc.files.iter().filter(|f| f.binary).count(),
          "warnings": warnings,
        }
      });
      let pretty =
        serde_json::to_string_pretty(&document).expect("machine document serializes: constructed from JSON values");
      write_stdout(format!("{pretty}\n").as_bytes())?;
    }
  } else if out_path != "-" {
    let c = args.color.enabled(std::io::stdout().is_terminal());
    let (bold, green, red, reset) = if c { ("\x1b[1m", "\x1b[32m", "\x1b[31m", "\x1b[0m") } else { ("", "", "", "") };
    let line = format!(
      "Wrote {bold}{out_path}{reset} ({} files, {green}+{}{reset} {red}\u{2212}{}{reset}, {} commits)\n",
      doc.files.len(),
      doc.additions(),
      doc.deletions(),
      doc.commits.len()
    );
    write_stdout(line.as_bytes())?;
  }
  Ok(())
}

fn fail(e: &CliError, machine: bool, color: ColorMode) -> ! {
  if machine {
    let pretty = serde_json::to_string_pretty(&e.to_machine_json())
      .expect("error document serializes: constructed from JSON values");
    // In machine mode the error document IS the data: stdout, then the code.
    let _ = std::io::stdout().write_all(format!("{pretty}\n").as_bytes());
  } else {
    let c = color.enabled(std::io::stderr().is_terminal());
    let (red, reset) = if c { ("\x1b[31m", "\x1b[0m") } else { ("", "") };
    eprintln!("{red}error:{reset} {e}");
  }
  std::process::exit(e.code());
}

fn main() {
  let argv: Vec<String> = std::env::args().skip(1).collect();
  // Machine mode: --json anywhere in argv, or stdout not a terminal. Decided
  // before parsing so that parse errors themselves honor the contract.
  let machine = argv.iter().any(|a| a == "--json") || !std::io::stdout().is_terminal();

  let invocation = match parse_args(&argv, machine) {
    Ok(inv) => inv,
    Err(e) => fail(&e, machine, ColorMode::Auto),
  };

  match invocation {
    Invocation::Help { topic } => {
      let text = match topic.as_deref() {
        None | Some("help") => HELP,
        Some("exitcodes") => HELP_EXITCODES,
        Some(other) => {
          let e = CliError::Usage { message: format!("unknown help topic {other:?}; topics: exitcodes") };
          fail(&e, machine, ColorMode::Auto);
        }
      };
      let _ = write_stdout(format!("{text}\n").as_bytes());
    }
    Invocation::Version => {
      let _ = write_stdout(format!("packdiff {}\n", env!("CARGO_PKG_VERSION")).as_bytes());
    }
    Invocation::Run(args) => {
      packdiff::set_verbose(args.verbose);
      if let Err(e) = run(&args, machine) {
        fail(&e, machine, args.color);
      }
    }
  }
}
