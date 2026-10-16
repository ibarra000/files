#!/bin/sh
# The checks every change must pass, run by `.githooks/pre-push` and by CI.
#
#     sh tools/check.sh          everything, including the test suite
#     sh tools/check.sh --fast   everything except the test suite
#
# Run from anywhere inside the repository. Each failure prints how to fix it,
# and every check runs even after one fails, so a single pass lists them all.

set -u

cd "$(git rev-parse --show-toplevel)" || exit 2

fast=0
for arg in "$@"; do
    case "$arg" in
        --fast) fast=1 ;;
        *) echo "usage: sh tools/check.sh [--fast]" >&2; exit 2 ;;
    esac
done

failed=0

step() {
    printf '\n==> %s\n' "$1"
}

fail() {
    printf 'FAILED: %s\n' "$1"
    failed=1
}

# Where source lives. The scans below never look at `target/` or `ueli/`.
SOURCES="src tests benches build.rs"

# --- formatting -------------------------------------------------------------

step "rustfmt"
if ! cargo fmt --all -- --check; then
    fail "code is not formatted. Fix: cargo fmt --all"
fi

# --- lints ------------------------------------------------------------------

# The lint levels themselves, including `unsafe_code` and
# `undocumented_unsafe_blocks`, live in the `[lints]` table of Cargo.toml.
step "clippy"
if ! cargo clippy --all-targets --locked -- -D warnings; then
    fail "clippy reported problems. Fix them, or see docs/CODE_STANDARDS.md#unsafe-code"
fi

# --- unsafe allowlist -------------------------------------------------------

# `unsafe_code` is denied crate-wide, so a file can only use `unsafe` by
# opting out. Every opt-out must be listed, with a reason, in the allowlist.
step "unsafe allowlist"
allowlist=tools/unsafe-allowlist.txt
listed=$(sed -e 's/#.*//' -e 's/[[:space:]]*$//' -e '/^$/d' "$allowlist" | sort -u)
present=$(grep -rlE '#!?\[allow\(unsafe_code\)\]' $SOURCES | tr '\\' '/' | sort -u)

unlisted=$(printf '%s\n' "$present" | while read -r f; do
    [ -n "$f" ] || continue
    printf '%s\n' "$listed" | grep -qxF "$f" || printf '%s\n' "$f"
done)
if [ -n "$unlisted" ]; then
    printf '%s\n' "$unlisted" | sed 's/^/  not in the allowlist: /'
    fail "unsafe code outside the allowlist. Use a safe API, or add the file to $allowlist with a reason (see docs/CODE_STANDARDS.md#unsafe-code)"
fi

stale=$(printf '%s\n' "$listed" | while read -r f; do
    [ -n "$f" ] || continue
    if [ ! -f "$f" ] || ! grep -qE '#!?\[allow\(unsafe_code\)\]' "$f"; then
        printf '%s\n' "$f"
    fi
done)
if [ -n "$stale" ]; then
    printf '%s\n' "$stale" | sed 's/^/  listed but no longer opts out: /'
    fail "the allowlist names files that no longer use unsafe. Remove those lines from $allowlist"
fi

# --- naming -----------------------------------------------------------------

# rustc's `nonstandard_style` lints (denied in Cargo.toml) cover case. These
# cover the house rules it cannot see. See docs/CODE_STANDARDS.md#naming.
step "naming"
getters=$(grep -rnE '\bfn get_[a-z0-9_]*' $SOURCES)
if [ -n "$getters" ]; then
    printf '%s\n' "$getters"
    fail "functions are not named get_*. Name the thing returned: fn get_path() becomes fn path()"
fi

test_prefix=$(grep -rnE '\bfn test_[a-z0-9_]*[[:space:]]*\(' $SOURCES)
if [ -n "$test_prefix" ]; then
    printf '%s\n' "$test_prefix"
    fail "tests are named as sentences about behavior, without a test_ prefix: fn test_parse() becomes fn an_empty_line_parses_as_no_query()"
fi

# --- vocabulary -------------------------------------------------------------

# The product is general-purpose, so names and text use general words: "search
# input", "folder", "flat share", "viewer". The only exceptions are legacy
# values that the config migration must still read from old files.
step "vocabulary"
VOCAB='custpro|custompro|avwin|(^|[^a-z_])jobs?([^a-z_]|$)'
words=$(grep -rniE "$VOCAB" $SOURCES assets docs README.md CONTRIBUTING.md 2>/dev/null \
    | grep -vE '^src/config/(migrate|file)\.rs:[0-9]+:.*(job-folder|avwin)')
if [ -n "$words" ]; then
    printf '%s\n' "$words"
    fail "organization-specific words. Use the general terms in docs/CODE_STANDARDS.md#vocabulary"
fi

# --- tests ------------------------------------------------------------------

if [ "$fast" -eq 1 ]; then
    step "tests (skipped with --fast; CI runs them)"
else
    step "tests"
    if ! cargo test --locked; then
        fail "tests failed. Run: cargo test"
    fi
fi

echo
if [ "$failed" -ne 0 ]; then
    echo "Some checks failed; see FAILED lines above."
    exit 1
fi
echo "All checks passed."
