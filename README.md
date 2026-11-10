# data-koin: all collected coin data (raw on-chain transactions)

Every recorded pool: every transaction (incl. failed) of its first 6 h, raw getTransaction JSON (zstd JSONL), plus
early-buyer wallet history, token supplies and the pool lists. Archive = tar of raw*/ and arsip/, zstd level 19,
split into 90 MB parts (GitHub file limit).

Restore:
    cat data_koin.tar.zst.part.* > data_koin.tar.zst
    sha256sum -c SHA256_full_archive.txt
    zstd -d --long=27 data_koin.tar.zst -o data_koin.tar && tar -xf data_koin.tar
    sha256sum -c MANIFEST_data_koin.sha256      # every original data file
