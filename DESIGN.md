---
name: SERAPH
description: A quiet terminal workbench for one primary coding agent and visible parallel specialists.
colors:
  canvas: "#141414"
  panel: "#242424"
  text: "#e1e1e1"
  muted: "#6c6c6c"
  section-label: "#787878"
  command-blue: "#7aa2f7"
  agent-violet: "#bb9af7"
  connected-green: "#9ece6a"
  failure-red: "#f7768e"
  idle-amber: "#e0af68"
  model-teal: "#1abc9c"
  border: "#505058"
  divider: "#585858"
typography:
  body:
    fontFamily: "inherit"
    fontWeight: 400
  title:
    fontFamily: "inherit"
    fontWeight: 700
  label:
    fontFamily: "inherit"
    fontWeight: 400
spacing:
  cell: "1 cell"
  compact-gap: "2 cells"
  shell-row: "1 row"
  composer-height: "3 rows"
components:
  composer:
    backgroundColor: "{colors.canvas}"
    textColor: "{colors.text}"
    height: "{spacing.composer-height}"
  agent-selected:
    backgroundColor: "{colors.panel}"
    textColor: "{colors.text}"
  grok-dock:
    backgroundColor: "{colors.canvas}"
    textColor: "{colors.muted}"
---

# Design System: SERAPH

## Overview

**Creative North Star: "The Quiet Agent Workbench"**

SERAPH is a full-screen terminal workspace that keeps the primary conversation dominant while making parallel work continuously legible. Its visual language is deliberately 95% Grok Build: GrokNight theme and glyph primitives plus Dock V2 are ported directly from Grok Build, then modified for SERAPH’s agents and shared tasks. The remaining 5% comes from Prime Agent: Down opens a dedicated roster where agent ownership, status, prompt, and result preview scan as one system.

The interface is dense without feeling crowded. Color is reserved for identity, state, and action; structure comes from terminal rows, alignment, whitespace, and sparse borders rather than panels around every region.

**Key Characteristics:**

- Near-black full-screen canvas with no ornamental backdrop.
- Flat, dominant transcript above a bounded live subagent-and-task dock.
- Empty dock sections disappear; visible sections become keyboard-focusable from the composer.
- Rounded composer and help chrome; roster sections use top rules only.
- Agent state stays visible in the shell and expands into a keyboard-first roster.
- Terminal-native typography, spacing, glyphs, and motion.

## Colors

GrokNight neutrals carry the workspace; restrained TokyoNight accents encode action and state.

### Primary

- **Command Blue** (`#7aa2f7`): User prompt chevrons and navigation actions such as “all agents” and “back.”

### Secondary

- **Agent Violet** (`#bb9af7`): Agent identity, running state, SERAPH help branding, and reasoning-effort selection.
- **Model Teal** (`#1abc9c`): The active model label in the footer and nowhere else.

### Tertiary

- **Connected Green** (`#9ece6a`): Main-agent presence, completed agents, and system notices.
- **Failure Red** (`#f7768e`): Failed agents and error messages.
- **Idle Amber** (`#e0af68`): Unknown, waiting, or otherwise non-terminal agent state.

### Neutral

- **Canvas Black** (`#141414`): The uninterrupted application background.
- **Selection Charcoal** (`#242424`): Selected roster rows and subtle border structure.
- **Primary Text** (`#e1e1e1`): Transcript copy, titles, prompts, and selected content.
- **Muted Text** (`#6c6c6c`): Paths, counts, account state, hints, result previews, and supporting copy.
- **Section Label** (`#787878`): Bold Dock V2 header labels and the selected subagent’s open indicator.
- **Composer Border** (`#505058`): The rounded prompt outline.
- **Footer Divider** (`#585858`): Compact separators between footer controls.

**The Status-Only Color Rule.** Accents identify action, agent identity, or runtime state; they do not decorate empty space.

## Typography

**Display Font:** Inherited terminal typeface

**Body Font:** Inherited terminal typeface

**Label/Mono Font:** Inherited terminal typeface

**Character:** SERAPH does not choose or bundle a font. It relies on the user’s terminal monospace and creates hierarchy with weight, glyphs, alignment, and color.

### Hierarchy

- **Title** (bold, terminal-defined size): SERAPH, All agents, main, and agent identifiers.
- **Body** (regular, terminal-defined size): Conversation content, agent prompts, notices, and previews.
- **Label** (regular, terminal-defined size): Counts, status values, account/model metadata, effort, and shortcut hints.

**The Terminal Owns Type Rule.** Never introduce a bundled face or a separate size scale into these surfaces; preserve the terminal’s metrics.

## Layout

The chat shell is a five-band vertical layout: a one-row header, a transcript that consumes all remaining height, a variable-height Grok Dock V2, a three-row composer, and a one-row footer. Empty dock sections—including Subagents—consume no rows. Each nonempty expanded section adds one header, at most two data rows, and one overflow row; a collapsed section keeps only its header. Dock height, rendered rows, and keyboard cursor order come from the same donor-derived row walk. The dock height is capped against terminal height so at least one transcript row remains. The header pairs project identity on the left with main and up to four agent indicators on the right; overflow becomes a muted `+N`. The transcript auto-scrolls to its latest wrapped line.

The All Agents surface uses a two-row header, a Current project region at 52% of the remaining body, an Other projects region filling the balance, and a one-row footer. The main conversation is always the first row. Agent rows window around the current selection, and a selected agent with a result gains one indented preview row. Alignment gaps expand to terminal width; when content cannot fit, the layout preserves at least one separating cell.

**The Transcript Wins Rule.** Dynamic dock chrome may grow with live work, but it must preserve at least one transcript row.

## Elevation & Depth

There are no shadows. Depth is flat and structural: the selected agent row uses Selection Charcoal, the composer and help panel use rounded borders, and roster groups use a single top border. Everything else remains directly on Canvas Black.

**The Flat Workbench Rule.** Add hierarchy with a tonal selection or one border, never stacked cards or simulated elevation.

## Shapes

The composer and shortcut panel use Ratatui’s rounded border glyphs. Dock headers extend a straight divider across unused width; All Agents groups use only a straight top border. Transcript messages, headers, status rails, dock rows, and roster rows remain unboxed. Status is expressed with Grok glyph primitives: `●` for complete or failed, `○` for other idle agent states, the eight-frame `⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧` spinner for running agents, and `◆` / `◇` for active / other task states.

**The One Enclosure Rule.** The composer is the persistent enclosure; do not turn transcript messages or agent rows into cards.

## Components

### Shell Header

- **Structure:** One terminal row with bold SERAPH and muted project name on the left.
- **Agent Rail:** Connected main in green, then at most four compact agent IDs colored by status; excess agents collapse to `+N`.

### Transcript

- **User:** Bold blue `❯` followed by primary text; continuation lines indent by two cells.
- **Assistant:** Unadorned primary text.
- **System:** Green `●` with muted text.
- **Error:** Bold red `!` with red text.
- **Rhythm:** One blank row follows every message.

### Grok Dock V2

- **Headers:** Every empty section, including Subagents, is hidden. A nonempty header uses muted `▾` / `▸`, a bold bright-gray label, muted count, and a dim divider filling unused width.
- **Subagent Rows:** An expanded section shows at most two live children. Each row carries a colored status glyph and agent ID, a one-line prompt clipped at the terminal edge, and muted status when it fits. Additional children collapse to muted `▾ N more`.
- **Tasks Section:** Hidden when the shared task total is zero. Otherwise its expanded state shows at most two task rows and muted `▾ N more` overflow.
- **Task Rows:** Pair the task glyph and `#id` with a one-line subject; owner or `unclaimed` plus status aligns right. Active is violet `◆`, completed green `●`, failed red `●`, and other states amber `◇`.
- **Focus:** Tab on an empty composer focuses the first visible dock item. Up/Down or `j`/`k` follows the shared visible-row order; overflow rows are not focus targets. The focused row uses Selection Charcoal.
- **Activation:** Enter toggles a Subagents or Tasks header. Enter on a subagent row opens that child in the Prime-style All Agents view and shows `[↗]` while focused. Esc or Tab returns focus to the composer.
- **Behavior:** Task state refreshes cross-process from shared SQLite without entering conversation context. Down or Ctrl+G still opens the Prime-style All Agents view directly.

### Composer

- **Shape:** Rounded terminal border in Composer Border.
- **Size:** Fixed at three terminal rows.
- **Content:** Primary text on Canvas Black, muted “Ask SERAPH…” placeholder, and a bold blue `❯` title.
- **Behavior:** Enter sends; Alt+Enter inserts a newline.

### Status Footer

- **Left:** Muted account or signed-out state.
- **Right:** Teal model, violet effort selector, and muted help/account hints separated by gray vertical rules.

### All Agents Roster

- **Header:** Bold title plus muted connected count.
- **Current Project:** Main conversation first, then agent ID, fixed-width status, and prompt in a single row.
- **Selection:** The full selected row uses Selection Charcoal; status color remains intact.
- **Preview:** A selected completed result exposes only its first line, indented and muted.
- **Other Projects:** A top-ruled empty state states that no external agents are connected.
- **Navigation:** Up/Down or `j`/`k` selects; Home/End jumps; Esc, Left, or `q` returns.

### Shortcut Panel

- **Shape:** Rounded border in Selection Charcoal over Canvas Black.
- **Content:** Violet SERAPH title followed by plain keyboard-command pairs.
- **Behavior:** `?` toggles it over the transcript; Esc closes it.

## Do's and Don'ts

### Do:

- **Do** hide every empty dock section and expose at most two rows per expanded section.
- **Do** derive dock height, rendering, and cursor order from one shared visible-row walk.
- **Do** use the exact status mapping: violet running, green completed, red failed, amber other.
- **Do** keep shared SQLite task updates out of conversation context.
- **Do** preserve terminal-cell alignment, keyboard navigation, and the selected agent’s one-line result preview.
- **Do** use rounded borders only for the composer and shortcut panel.

### Don't:

- **Don't** box transcript messages or every agent row into cards.
- **Don't** use accent colors as decoration or assign a second meaning to a status color.
- **Don't** add shadows, gradients, raster decoration, or a bundled font to these terminal surfaces.
- **Don't** let expanded agent detail displace the primary chat shell; keep it in the dedicated All Agents view.
