#!/usr/bin/env bash
# Shared test runner helpers for justfile targets.
# Parses cargo-nextest Summary lines to extract per-section and overall counts.

set -uo pipefail

declare -a SECTIONS=()
declare -A SECTION_PASSED=()
declare -A SECTION_FAILED=()
declare -A SECTION_STATUS=()
OVERALL_RC=0

run_section() {
    local name="$1"; shift
    SECTIONS+=("$name")

    echo ""
    echo "================================================================"
    echo "  $name"
    echo "================================================================"

    # Run the command, tee output so we can parse the Summary line
    local tmpfile
    tmpfile=$(mktemp)
    "$@" 2>&1 | tee "$tmpfile"
    local rc=${PIPESTATUS[0]}

    # Parse nextest Summary line: "Summary [...] N tests run: P passed, F failed, S skipped"
    # Also handles "(X slow)" in the passed count
    local summary_line
    summary_line=$(grep -E '^\s*Summary' "$tmpfile" | tail -1)
    rm -f "$tmpfile"

    local passed=0 failed=0
    if [[ -n "$summary_line" ]]; then
        passed=$(echo "$summary_line" | grep -oP '\d+(?=\s+passed)' || echo 0)
        failed=$(echo "$summary_line" | grep -oP '\d+(?=\s+failed)' || echo 0)
    fi

    SECTION_PASSED["$name"]=${passed:-0}
    SECTION_FAILED["$name"]=${failed:-0}

    if [ $rc -eq 0 ]; then
        SECTION_STATUS["$name"]="PASS"
    else
        SECTION_STATUS["$name"]="FAIL"
        OVERALL_RC=1
    fi
}

print_summary() {
    local total_passed=0 total_failed=0

    echo ""
    echo "================================================================"
    echo "  OVERALL SUMMARY"
    echo "================================================================"
    for section in "${SECTIONS[@]}"; do
        local p=${SECTION_PASSED[$section]:-0}
        local f=${SECTION_FAILED[$section]:-0}
        total_passed=$((total_passed + p))
        total_failed=$((total_failed + f))

        local icon
        if [ "${SECTION_STATUS[$section]}" = "PASS" ]; then icon="PASS"; else icon="FAIL"; fi
        printf "  %-6s %-20s %d passed, %d failed\n" "$icon" "$section" "$p" "$f"
    done
    echo "----------------------------------------------------------------"
    printf "  %-6s %-20s %d passed, %d failed\n" \
        "TOTAL" "" "$total_passed" "$total_failed"
    echo "================================================================"

    exit $OVERALL_RC
}
