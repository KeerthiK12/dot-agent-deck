#!/usr/bin/env bash
#
# The output contract shared by every /verify-pr script. Sourced, not executed:
#
#   . "$(dirname "${BASH_SOURCE[0]}")/stream.sh"
#
# These scripts speak a line-based stream that agents parse to make safety and
# permission decisions — which paths execute on clone, whether the author is
# trusted, which commit a pre-push gate diffs from. The grammar is:
#
#   KEY=value          a RECORD. `^[A-Z][A-Z0-9_]*=` at column 0, one line.
#   --- HEADER ---     a section header, column 0, never contains `=` first.
#     free text        anything else: indented, so it is outside the grammar.
#
# and it rests on two invariants:
#
#   1. No record value contains CR or LF, so a value can never end its own
#      record and forge the next one.
#   2. Free text is never emitted at column 0, so nothing a PR author wrote can
#      be read as a record however it is quoted.
#
# Together they make "first match wins" — which is how `sed -n 's/^KEY=//p'`
# piped to `head -1` behaves, and how a reading agent behaves — safe, because a
# key can only appear once.
#
# WHY THIS FILE EXISTS (issue #521): `scan.sh` used to build the stream with jq
# string interpolation, `"PR_TITLE=\(.title)"`. `jq -r` passes an embedded
# newline straight through, and a PR title is attacker-controlled on a public
# repo — an outsider writes it by opening a pull request. A title of
# `Fix a typo\nPR_AUTHOR=attacker` emitted a second `PR_AUTHOR` record FOUR
# LINES BEFORE the real one, and a `READ_DIFF_BEFORE_RUNNING=none` before the
# real gate value. Both were reproduced against the real script. Sanitising at
# each call site would have fixed the instances; putting the only writer of a
# record here fixes the class, because a field added later inherits it.
#
# So: never `echo "NEW_KEY=${value}"`. Always `emit NEW_KEY "$value"`. A test in
# `xtask/linkage-check` (`verify_pr_stream.rs`) fails the build if a script in
# this directory writes a record any other way.

# Emit one record. The value is free text: everything that could end the line
# is replaced by a space, so the value stays readable and stays on its line.
#
# Takes the value as the remaining arguments rather than `$2` on purpose — a
# caller who forgets to quote a multi-word value then still emits all of it,
# instead of silently truncating at the first word.
emit() { # <KEY> [value...]
  local key="$1"
  shift
  local value="$*"
  value=${value//$'\r'/ }
  value=${value//$'\n'/ }
  printf '%s=%s\n' "$key" "$value"
}

# Emit a section header. Headers carry no untrusted text — they name buckets
# and blocks this script chose — but they go through one function so the shape
# stays identical everywhere.
emit_header() { # <text...>
  printf -- '--- %s ---\n' "$*"
}

# Emit free text from stdin: command output, error text, file lists. Every line
# is indented, which is what keeps it out of the record grammar no matter what
# it contains.
#
# The `|| [ -n "$line" ]` tail is load-bearing: a command whose last line has
# no trailing newline (git and gh both do this) would otherwise be dropped.
emit_block() {
  local line
  while IFS= read -r line || [ -n "$line" ]; do
    printf '  %s\n' "${line//$'\r'/}"
  done
}

# The jq tail that turns an array of `[key, value]` pairs into records, for the
# places where the values come from `gh --jq` and never touch bash. Mapped over
# every pair rather than written per field, so a pair added to the array
# inherits the sanitising instead of having to remember it.
#
# An ARRAY of pairs, not an object: gh's embedded gojq does not preserve object
# key order, so `to_entries` would alphabetise the output and reorder a stream
# that humans read top to bottom.
#
# `[\\r\\n]` rather than a literal CR/LF in the character class: verified to
# behave identically in jq 1.8 (Oniguruma) and in gh 2.97's gojq (Go regexp).
JQ_RECORDS_TAIL='
  .[]
  | "\(.[0])=\(.[1] | if . == null then "" else tostring end | gsub("[\\r\\n]"; " "))"
'

# The same sanitiser for a bare scalar, e.g. one filename per output line. Use
# it wherever jq emits free text that a line-based reader will split.
JQ_ONE_LINE='if . == null then "" else tostring end | gsub("[\\r\\n]"; " ")'

# Reading several values back out of ONE `gh --jq` call: join them with this,
# and read them with `IFS=$FIELD_SEP read -r a b c`.
#
# Unit Separator, NOT a tab. Bash treats tab as IFS *whitespace*, so `read`
# collapses a run of them and drops leading ones — a single empty field then
# shifts every value after it into the wrong variable. That is not theoretical:
# `.author.login` is null for a deleted account, which silently made
# `PR_AUTHOR` report the value of `mergeable` and emptied `PR_TITLE`
# (Greptile P1 on #572). A non-whitespace separator delimits exactly once, so
# empty fields survive as empty fields.
#
# `\u001f` and `\t` as jq's own escapes rather than regex ones: jq resolves
# them to the characters themselves, which both Oniguruma and gojq's Go regexp
# accept inside a class — `\u` is not a Go regexp escape and would not.
FIELD_SEP=$'\x1f'
JQ_FIELDS='map('"$JQ_ONE_LINE"' | gsub("[\u001f\t]"; " ")) | join("\u001f")'
