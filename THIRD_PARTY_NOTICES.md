# Third-party notices

## Prime Agent persistent Python REPL

`python/kernel.py` is derived from Prime Agent's `src/rlm/repl.py` at commit
`9f5edc192cfe3d4737205a2f551d2b6b6e34fe09`.

MIT License

Copyright (c) 2025 Mario Zechner
Copyright (c) 2026 Prime Intellect

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## xAI Grok Build terminal UI

The terminal shell in `src/tui.rs` and the modified GrokNight theme, glyph, and
Dock V2 production code in `src/grok_ui.rs` are adapted from
`xai-grok-pager-render/src/theme/groknight.rs`,
`xai-grok-pager-render/src/glyphs.rs`, and `xai-grok-pager/src/views/dock.rs`
in Grok Build at commit
`bb7f39d5858cbf5e00de639367f59debbdcb0138`, copyright 2023-2026 SpaceXAI,
under the Apache License 2.0:
`LICENSES/GROK-BUILD-APACHE-2.0.txt`.

SERAPH changes the product identity, backend, data model, composer shortcuts,
agent roster, and layout to integrate Codex and Prime-style navigation.

## Prime Agent multi-agent navigation

The down-arrow All Agents entry point and current-project/other-project visual
split in `src/tui.rs` are adapted from Prime Agent's Agents View at commit
`4e42fab2ce0c486cd6da0237b56b9b7787d06bfd`, copyright 2025 Mario
Zechner and 2026 Prime Intellect, under the MIT License reproduced above.

## Pi authentication runtime

`auth/pi-auth.mjs` delegates OpenAI Codex OAuth, credential persistence, and
refresh to `@earendil-works/pi-coding-agent` version 0.84.4. Pi is copyright
2025 Mario Zechner and is used under the MIT License reproduced above.

The authentication pane and its browser, manual fallback, device-code, and
cancel states in `src/tui.rs` are adapted from Prime Agent's
`login-dialog.ts`, `oauth-selector.ts`, and `auth-flows.ts` at commit
`4e42fab2ce0c486cd6da0237b56b9b7787d06bfd` under the same MIT License.

## OpenAI Codex apply-patch core

`src/edit_patch.rs` derives its strict update parser, source-file line-ending
model, and exact-match preflight vocabulary from OpenAI Codex apply-patch at
commit `2c3bf4ea793aa5c590932553d242a287380e9cec`, copyright 2025 OpenAI,
under the Apache License 2.0. Its copyright notice is in
`LICENSES/CODEX-APACHE-2.0.txt`; the full license text is in
`LICENSES/GROK-BUILD-APACHE-2.0.txt`.
