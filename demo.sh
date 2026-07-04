#!/usr/bin/env bash
# Build packdiff, create a small sample repo with two branches, and render its
# diff to out/demo.html. Idempotent: everything lands under gitignored dirs.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release -p packdiff

SAMPLE=out/demo-repo
rm -rf "$SAMPLE"
mkdir -p "$SAMPLE"

g() { git -C "$SAMPLE" -c user.name=Demo -c user.email=demo@example.com "$@"; }

git -C "$SAMPLE" init -q
git -C "$SAMPLE" symbolic-ref HEAD refs/heads/main

cat > "$SAMPLE/greet.py" <<'EOF'
def greet(name):
    return f"Hello, {name}!"

def farewell(name):
    return f"Bye, {name}."
EOF
printf 'A demo repository for packdiff.\n' > "$SAMPLE/README.md"
g add -A
g commit -qm "initial layout"

g checkout -qb feature
cat > "$SAMPLE/greet.py" <<'EOF'
def greet(name, excited=False):
    suffix = "!!!" if excited else "!"
    return f"Hello, {name}{suffix}"

def farewell(name):
    return f"Bye, {name}."
EOF
g add -A
g commit -qm "greet: optional excitement"

printf 'def shout(s):\n    return s.upper()\n' > "$SAMPLE/shout.py"
g add -A
g commit -qm "add shout helper"

./target/release/packdiff main feature -C "$SAMPLE" -o out/demo.html
echo "Open out/demo.html in a browser (file:// is fine). Click a diff line to comment."
