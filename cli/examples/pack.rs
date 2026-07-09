//! The library API end to end: pack a repository's diff into `review.html`.
//!
//! Run from anywhere inside a git repository:
//!
//! ```console
//! $ cargo run --example pack -- main
//! $ cargo run --example pack -- v1.0.0 v1.1.0
//! ```

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let args: Vec<String> = std::env::args().skip(1).collect();
  let (base, head) = match args.as_slice() {
    [base] => (base.as_str(), "HEAD"),
    [base, head] => (base.as_str(), head.as_str()),
    _ => {
      eprintln!("usage: cargo run --example pack -- <BASE> [HEAD]");
      std::process::exit(2);
    }
  };
  let opts = packdiff::PackOptions::new(".", base, head);
  let out = packdiff::pack(&opts, &())?; // &(): no progress reporting
  std::fs::write("review.html", &out.html)?;
  println!(
    "review.html: {} files, +{} \u{2212}{}, {} commits",
    out.document.files.len(),
    out.document.additions(),
    out.document.deletions(),
    out.document.commits.len()
  );
  Ok(())
}
