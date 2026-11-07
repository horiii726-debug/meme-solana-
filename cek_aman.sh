#!/bin/bash
# Mint safety check (read-only) for every coin in the given data files -> JSONL {"k":"aman",...}
# Unsafe = mint authority or freeze authority still set, or a Token-2022 extension that can block/tax/take tokens
# (transferHook, transferFeeConfig, permanentDelegate, nonTransferable, defaultAccountState, pausable). Unknown = unsafe.
# Note: this is the CURRENT on-chain state, not the state at launch time.
set -a; . /root/sol/.rpc.env; set +a
OUT=$1; shift
TMP=$(mktemp -d)
for f in "$@"; do zcat -f "$f" | grep '"k":"create"' | jq -r '.mint'; done | sort -u > $TMP/mints
split -l 100 $TMP/mints $TMP/part_
for p in $TMP/part_*; do
  ids=$(jq -R . $p | jq -s -c .)
  curl -s "$SOL_RPC" -H 'content-type: application/json' -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getMultipleAccounts\",\"params\":[$ids,{\"encoding\":\"jsonParsed\"}]}" \
  | jq -c --argjson ids "$ids" '.result.value as $v | range(0; $ids|length) as $i | ($v[$i]) as $a |
      ([$a.data.parsed.info.extensions[]?.extension]) as $ext |
      ([ (if $a == null then "akun tidak ada" else empty end),
         (if $a != null and $a.data.parsed.info.mintAuthority != null then "mint authority aktif" else empty end),
         (if $a != null and $a.data.parsed.info.freezeAuthority != null then "freeze authority aktif" else empty end),
         ($ext[] | select(. == "transferHook" or . == "transferFeeConfig" or . == "permanentDelegate" or . == "nonTransferable" or . == "defaultAccountState" or . == "pausable")) ]) as $bad |
      {k: "aman", mint: $ids[$i], ok: ($bad | length == 0), alasan: $bad, program: $a.owner}' >> $OUT
  sleep 1
done
rm -rf $TMP
