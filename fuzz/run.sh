#!/bin/sh
# Runs every fuzz target at the same time, each for the given number of seconds (default 60),
# and reports which failed. Use it through `make fuzz`. It needs the nightly toolchain and cargo-fuzz:
#   rustup toolchain install nightly && cargo install cargo-fuzz --locked
#
# The corpus of each target is kept under fuzz/corpus/ and grows from run to run. An input that
# fails is saved under fuzz/artifacts/<target>/; replay it with
#   cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<file>
set -eu
seconds=${1:-60}
cd "$(dirname "$0")"

cargo +nightly fuzz build
targets=$(cargo +nightly fuzz list)

# Seed each corpus from the sample logs and the example rule files, so the fuzzer starts from
# valid input instead of having to discover the formats by itself. The first byte of a
# rule_engine or rule_file input is a selector: "0" means json / YAML, "1" means combined.
for t in $targets; do mkdir -p "corpus/$t"; done
n=0
while IFS= read -r line; do
	n=$((n + 1))
	printf '%s' "$line" >"corpus/parse_json/seed-$n"
done <../sample.log
n=0
while IFS= read -r line; do
	n=$((n + 1))
	printf '%s' "$line" >"corpus/parse_combined/seed-$n"
done <../sample.combined.log
{ printf 0; cat ../sample.log; } >corpus/rule_engine/seed-json
{ printf 1; cat ../sample.combined.log; } >corpus/rule_engine/seed-combined
for f in ../rules.example.yaml ../examples/*.yaml; do
	{ printf 0; cat "$f"; } >"corpus/rule_file/seed-$(basename "$f")"
done
cp ../tests/data/secrets.env corpus/secrets_file/seed-corpus

logs=$(mktemp -d)
for t in $targets; do
	(
		status=0
		cargo +nightly fuzz run "$t" -- -max_total_time="$seconds" -timeout=10 -max_len=8192 \
			>"$logs/$t.log" 2>&1 || status=$?
		echo "$status" >"$logs/$t.status"
	) &
done
wait

status=0
for t in $targets; do
	if [ "$(cat "$logs/$t.status")" = 0 ]; then
		echo "ok    $t  $(grep -o 'Done [0-9]* runs' "$logs/$t.log" | tail -1)"
	else
		echo "FAIL  $t"
		tail -25 "$logs/$t.log"
		status=1
	fi
done
exit $status
