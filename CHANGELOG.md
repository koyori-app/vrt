# Changelog

リリース（`cli-v*` タグ）を切るときは、この Unreleased 節をリリースノートへ
転記すること。特に **破壊的変更** はデプロイ後の初回ビルドで顕在化するため、
リリースノート本文の先頭に載せる。

## Unreleased

### 破壊的変更

- **スクリーンショット名の前後空白を拒否するようになった**（従来は保存時に
  黙って trim）。既存プロジェクトの story 名（CSF の `title` / named export の
  `name`）に前後空白が付いていると、デプロイ後の初回ビルドが 400 / `failed` で
  落ちる。直し方は story 定義から前後空白を取り除くこと。trim をやめた理由は
  README「storybook モード」の破壊的変更の節を参照。
- **スクリーンショット名の制御文字（NUL・エスケープ・改行・タブ等）を拒否する
  ようになった**（従来は素通りし、NUL 入りは DB 書き込みで 500 になっていた）。
- **スクリーンショット名は受理時に Unicode NFC へ正規化して保存・比較する**。
  非 ASCII 名を NFD で送っていたクライアントは、保存される名前が NFC 形に
  変わる（見た目は同一）。既存 baseline が NFD 名を含む場合、次のビルドで
  その名前は一度 removed + added として報告される。

### 修正

- CLI の差分選別が非 ASCII / 改行入りパスの変更で常に全撮影へ倒れていたのを
  修正（`git diff` の C-quoting を NUL 区切りで受けて解消）。
- CLI の finalize が `--only-changed` 無しのとき `expected_baseline_commit_sha`
  を送らず捨てていたのを修正。
