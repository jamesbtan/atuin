#!/usr/bin/env bash
set -euo pipefail

# 1. Ensure rustc has generated fresh dep-info (.d) files for all targets and features.
#    Wait: if you have old .d files in target/debug/deps/ from deleted modules,
#    only look at .d files modified by the current build (or run `rm -f target/debug/deps/*.d` first).
#
#    Uncomment the two lines below if you want the script to refresh `.d` files automatically:
# rm -f target/debug/deps/*.d
# cargo check --workspace --all-targets --all-features

# 2. Compare all `.rs` files in `crates/*/src/` on disk against the `.rs` files
#    listed inside `target/debug/deps/*.d` (which rustc actually parsed/compiled).
#
#    `comm -23 A B` prints lines that appear in sorted list A (on disk)
#    but NOT in sorted list B (compiled by rustc).
comm -23 \
  <(find crates -path 'crates/*/src/*.rs' | sort) \
  <(cat target/debug/deps/*.d | tr ' ' '\n' | grep '^crates/.*/src/.*\.rs$' | sort -u)
