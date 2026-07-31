#!/usr/bin/env bash
# Assert that this package's crate-name is either free on crates.io or already
# ours.
#
# Why this exists (bd-ab0c67): `cargo publish --dry-run` packages and compiles
# locally and never attempts the upload, so it cannot fail for the reason you
# run it. It reported clean on this repo for the entire time the `mcp-cli` name
# was owned by an unrelated project, and it would report clean the day before a
# real publish failed. A control that is structurally incapable of returning the
# failing answer is not a control.
#
# Exit codes:
#   0  name is free, or is already ours (repository matches), or the registry
#      could not be reached (warn, do not fail)
#   1  name is taken by somebody else
#
# Runnable locally:  ./.github/scripts/assert-crate-name.sh
#
# The two overrides below exist so each branch can be exercised for real against
# the live registry rather than reasoned about:
#   CRATE_NAME=mcp-cli ./.github/scripts/assert-crate-name.sh
#     -> the taken-by-someone-else branch (this repo's actual history)
#   CRATE_NAME=mcp-cli CRATE_REPOSITORY=https://github.com/conikeec/mcp-probe.git \
#     ./.github/scripts/assert-crate-name.sh
#     -> the already-ours branch, which is what gates CI once we do publish
#   https_proxy=http://127.0.0.1:1 ./.github/scripts/assert-crate-name.sh
#     -> the registry-unreachable branch

set -euo pipefail

metadata=$(cargo metadata --no-deps --format-version 1)
# `--no-deps` on a single-package repo yields exactly one entry.
name=${CRATE_NAME:-$(jq -r '.packages[0].name' <<<"$metadata")}
ours=${CRATE_REPOSITORY:-$(jq -r '.packages[0].repository // ""' <<<"$metadata")}

echo "package name : $name"
echo "our repository: ${ours:-<unset>}"

body=$(mktemp)
trap 'rm -f "$body"' EXIT

# `|| true` and an empty-string default: on a transport failure curl already
# writes its own `000` to stdout, so an `|| echo 000` fallback would concatenate
# into a nonsense `000000` in the diagnostic below.
code=$(curl -sS -o "$body" -w '%{http_code}' --max-time 20 \
  -H "User-Agent: ${name}-ci (crate-name availability assert)" \
  "https://crates.io/api/v1/crates/${name}" 2>/dev/null || true)
code=${code:-000}

normalise() {
  # Compare repository URLs tolerantly: case, a trailing `.git`, and a trailing
  # slash are not meaningful differences.
  tr '[:upper:]' '[:lower:]' <<<"$1" | sed -e 's#\.git$##' -e 's#/$##'
}

case "$code" in
  404)
    echo "PASS: '$name' is not registered on crates.io."
    ;;
  200)
    theirs=$(jq -r '.crate.repository // ""' <"$body")
    echo "registered to : ${theirs:-<unset>}"
    if [[ -n "$ours" && -n "$theirs" && "$(normalise "$ours")" == "$(normalise "$theirs")" ]]; then
      echo "PASS: '$name' is registered to this repository."
    else
      echo "FAIL: '$name' on crates.io belongs to a different project."
      echo "      description: $(jq -r '.crate.description // "<none>"' <"$body")"
      echo "      newest     : $(jq -r '.crate.newest_version // "<none>"' <"$body")"
      echo
      echo "Publishing under this name is impossible. Rename the package (keeping"
      echo "the library name via [lib] name keeps 'use' paths working), or drop the"
      echo "publication metadata if this crate is git-consumed by design."
      exit 1
    fi
    ;;
  *)
    # A registry outage, a rate limit, or a network blip must not redden main:
    # this check answers a question that changes rarely, and a false red here
    # would block merges for a reason unrelated to the change under test.
    echo "WARN: could not reach crates.io (HTTP $code); skipping the assertion."
    ;;
esac
