# diri-i18n

diri's interface language. UI code names each message by id (`t("settings.tab.general")`) and the
text lives in JSON catalogs, so translating never touches UI code.

```
locales/
  en/<area>.json        canonical English, one file per UI area
  zh-Hans/<area>.json   Simplified Chinese, same files, same ids
```

`build.rs` compiles every file into the binary; a new area file needs no registration.

## Rules the tests enforce

- Each language has every English area file, with exactly the same ids.
- Ids are sorted, unique across all files, one `"id": "message"` per line.
- A translation keeps the English message's `{placeholder}` names.
- Every id the app passes to `t`/`tf` exists, and every id is still used (`diri-app` `i18n::tests`).

## Translating

- Edit `locales/zh-Hans/*.json` only; keep ids and `{placeholders}` as they are.
- Translate app chrome. Agent output, terminal text, file paths, commands and product names
  (diri, Claude Code, Codex, GitHub, SSH, …) stay as written.
- Simplified Chinese: full-width punctuation, one space between Chinese and Latin text or digits,
  no trailing `。` on buttons and short labels. Terms in use: agent 智能体, session 会话,
  worktree 工作树, recipe 模板, conversation 对话, Copy 拷贝, Duplicate 复制.

## Adding a language

Add a `Language` variant with its BCP 47 tag and native name in `src/lib.rs`, a
`locales/<tag>/` directory with every area file, and the tag to `CFBundleLocalizations` in
`assets/Info.plist` and `scripts/dev.sh`.

Preview a page in a language with the headless fixtures, for example:

```
DIRI_VISUAL_LANGUAGE=zh-Hans DIRI_VISUAL_SETTINGS_TAB=general DIRI_VISUAL_OUTPUT=/tmp/general.png \
  cargo test -p diri-app render_settings_shell_preview_screenshot -- --ignored
```
