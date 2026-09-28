#!/bin/bash
# Waits on GitHub Actions for a release source commit, so release.sh reuses
# work CI already does instead of redoing it on the release machine.
#
# Usage:
#   diri/scripts/await-ci.sh gates <sha>
#       Succeeds once the CI workflow's push run on <sha> has passed. That run
#       is the same clippy + workspace test pair release.sh used to repeat
#       locally, on the exact commit being released.
#   diri/scripts/await-ci.sh linux <sha> <out-dir>
#       Downloads the linux-packages-<sha> artifact from a Nightly run on <sha>,
#       dispatching one against main if none exists. The Linux package jobs
#       must have passed; unrelated Nightly jobs do not gate the download.
#
# Env overrides:
#   GH_REPO                  default cristicretu/diri
#   DIRI_CI_POLL_SECONDS     default 20
#   DIRI_CI_TIMEOUT_SECONDS  default 5400 (a fresh Nightly takes ~40 minutes)
set -euo pipefail

GH_REPO="${GH_REPO:-cristicretu/diri}"
POLL="${DIRI_CI_POLL_SECONDS:-20}"
DEADLINE=$((SECONDS + ${DIRI_CI_TIMEOUT_SECONDS:-5400}))
# Display-name prefix shared by the Nightly packaging job and its Ubuntu 24.04
# smoke. Renaming those jobs must update this.
LINUX_JOB_PREFIX="Linux package"

usage() {
    echo "usage: await-ci.sh gates <sha> | await-ci.sh linux <sha> <out-dir>" >&2
    exit 2
}

log() {
    echo "[$(date +%H:%M:%S)] $*"
}

sleep_or_timeout() {
    if [ "$SECONDS" -ge "$DEADLINE" ]; then
        echo "error: timed out waiting on GitHub Actions ($1)" >&2
        exit 1
    fi
    sleep "$POLL"
}

# Newest run of <workflow> on <sha> whose event passes <jq filter>, or empty.
newest_run() {
    gh run list -R "$GH_REPO" --workflow "$1" --commit "$2" -L 20 \
        --json databaseId,event,createdAt \
        --jq "[.[] | select($3)] | sort_by(.createdAt) | last | .databaseId // empty"
}

await_gates() {
    local sha="$1" run="" status conclusion
    while [ -z "$run" ]; do
        run="$(newest_run ci.yml "$sha" '.event == "push"')"
        [ -n "$run" ] || sleep_or_timeout "no CI push run for $sha yet"
    done
    log "CI run $run for $sha"
    while :; do
        read -r status conclusion < <(gh run view "$run" -R "$GH_REPO" \
            --json status,conclusion --jq '"\(.status) \(.conclusion)"')
        if [ "$status" = "completed" ]; then
            break
        fi
        sleep_or_timeout "CI run $run is $status"
    done
    if [ "$conclusion" != "success" ]; then
        echo "error: CI run $run on $sha concluded '$conclusion'" >&2
        echo "  https://github.com/$GH_REPO/actions/runs/$run" >&2
        exit 1
    fi
    log "CI passed on $sha"
}

# Prints "<run status> <linux job count> <linux jobs not yet completed> <linux jobs failed>".
linux_job_state() {
    gh run view "$1" -R "$GH_REPO" --json status,jobs --jq "
        [.jobs[] | select(.name | startswith(\"$LINUX_JOB_PREFIX\"))] as \$linux
        | \"\(.status) \(\$linux | length) \
\([\$linux[] | select(.status != \"completed\")] | length) \
\([\$linux[] | select(.status == \"completed\" and .conclusion != \"success\")] | length)\""
}

await_linux() {
    local sha="$1" out="$2" run status count pending failed
    # Pull-request runs name their artifact after the PR head, never a main
    # commit, so only scheduled and dispatched runs can carry this one.
    run="$(newest_run nightly.yml "$sha" '.event != "pull_request"')"
    if [ -z "$run" ]; then
        log "no Nightly run on $sha; dispatching one against main"
        gh workflow run nightly.yml -R "$GH_REPO" --ref main
        # The dispatched run builds main as of now. release.sh has already
        # proven main == $sha; if main moved since, no run on $sha appears and
        # this times out rather than shipping a different commit's packages.
        while [ -z "$run" ]; do
            sleep_or_timeout "the dispatched Nightly run never appeared for $sha (did main move?)"
            run="$(newest_run nightly.yml "$sha" '.event == "workflow_dispatch"')"
        done
    fi
    log "Nightly run $run for $sha: https://github.com/$GH_REPO/actions/runs/$run"

    # Dependent jobs are only listed once they are queued, so require both
    # Linux jobs to be present before trusting "none pending".
    while :; do
        read -r status count pending failed < <(linux_job_state "$run")
        if [ "$failed" -gt 0 ]; then
            echo "error: a Linux package job failed in Nightly run $run" >&2
            echo "  https://github.com/$GH_REPO/actions/runs/$run" >&2
            exit 1
        fi
        if [ "$count" -ge 2 ] && [ "$pending" -eq 0 ]; then
            break
        fi
        if [ "$status" = "completed" ]; then
            echo "error: Nightly run $run finished without passing Linux package jobs" \
                "(it may have been cancelled by a newer Nightly on main)" >&2
            exit 1
        fi
        sleep_or_timeout "Linux package jobs in run $run ($pending of $count pending)"
    done

    rm -rf "$out"
    mkdir -p "$out"
    # The artifact is downloadable as soon as its job uploads it, while the
    # rest of the Nightly run is still going.
    until gh run download "$run" -R "$GH_REPO" -n "linux-packages-$sha" -D "$out"; do
        rm -rf "$out"
        mkdir -p "$out"
        sleep_or_timeout "downloading linux-packages-$sha from run $run"
    done
    log "Linux packages for $sha in $out"
}

case "${1:-}" in
    gates)
        [ $# -eq 2 ] || usage
        await_gates "$2"
        ;;
    linux)
        [ $# -eq 3 ] || usage
        await_linux "$2" "$3"
        ;;
    *)
        usage
        ;;
esac
