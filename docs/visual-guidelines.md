# Prodex visual guidelines

## References

Inspected the public homepages and their linked stylesheets on 2026-09-29.
These are observations and a Prodex interpretation, not official brand manuals.

- [Cursor](https://cursor.com/): its UI sans family is Cursor Gothic. Borrow the
  restrained typography, clear hierarchy, and low-contrast surface separation.
- [SF Compute](https://sfcompute.com/): its font definitions include ABC Diatype
  Regular, Medium, and Bold; ABC Diatype Mono; and ABC Otto for display use.
  Borrow the typographic restraint and precise rules between content areas.
- [Parallel](https://parallel.ai/): the inspected wordmark is heavy and lowercase.
  The earlier wordmark drew on this weight; the current wordmark uses a geometric sans to match the Hyras mark.

## Application

The palette uses [Material 3 color roles](https://codelabs.developers.google.com/codelabs/apply-dynamic-color#3):
primary, surface, on-surface, and paired foreground/container colors. This is a
hand-picked Prodex palette implemented through CSS variables, not a generated
Material theme or a new component-library dependency. Success and warning extend
those roles for this app's task states.

| Role | Foreground | Pale container | Use |
| --- | --- | --- | --- |
| Primary / clear blue | `#246bce` | `#edf4ff` | Active tab, primary actions, switch, focus, running task icons |
| Success / green | `#287451` | `#edf7f0` | Connected/live dots and completed task icons |
| Warning / amber | `#8a5b15` | `#fff7e6` | Approval, retry icons, retry notification |
| Danger / red | `#b13e48` | `#fff0f1` | Errors, recovery, stop and disconnect actions |
| Primary text | `#252a30` | `#ffffff` | Titles and body text |
| Secondary text | `#626c76` | `#f8fafc` | Hints, metadata, expanded-card surface |

The wordmark stays black (`#000000`). White remains the dominant surface. Clear blue provides identity without competing
with status colors. Primary buttons use white text on clear blue; Add projects uses
clear blue text on a pale container. Use `#1956ae` for primary hover and `#eaf2ff`
for pale accent hover. Keep normal cards white; avoid gradients and decorative
button shadows. Decorative dividers use `#e3e8ec`, card outlines `#cdd5dc`, and
input/switch outlines `#818d98`.

Verified using the WCAG relative-luminance formula: white on primary is 5.16:1;
primary on its container is 4.67:1 (hover 4.58:1); status foreground/container pairs
are at least 5.17:1; muted text on the expanded surface is 5.11:1; input outlines
on white are 3.39:1. These meet the relevant [text contrast](https://www.w3.org/WAI/WCAG21/Understanding/contrast-minimum)
and [UI contrast](https://design-system.w3.org/settings/) thresholds for these
pairs. This is a palette check, not a full accessibility audit. Preserve icon
shapes, hover/accessibility labels, selected-tab underlines, and visible keyboard
focus so color is not the only signal.

Public Sans is the locally bundled UI family: 14px body text, 22px dialog headings,
and modest weight changes for hierarchy. The black “Prodex” wordmark uses
locally bundled Space Grotesk Regular at 24px, weight 400, upright, with -.7px tracking.
Its geometric forms and restrained details complement the Hyras mark.
The SIL license is bundled in `public/fonts/OFL-Space-Grotesk.txt`. The wordmark retains the accessible name “Prodex”.
Navigation uses a thin underline for selection. Keep existing task and project
interactions and internal Activity scrolling.

Public Sans is an open-source alternative, not either reference site's font.
Normal and italic variable Latin WOFF2 files are bundled in
`apps/desktop/public/fonts`, with the SIL Open Font License. Files were obtained
from the [Fontsource Public Sans distribution](https://fontsource.org/fonts/public-sans);
the license comes from the [Google Fonts source](https://github.com/google/fonts/tree/main/ofl/publicsans).
No remote font request is needed at runtime. Characters outside the Latin subset
use the Helvetica Neue / Arial / sans-serif fallback stack.

## Icons

Use the free stroke set from [Hugeicons](https://github.com/hugeicons/hugeicons),
through `@hugeicons/core-free-icons`. Import only the icons used in the app; the
small DOM renderer in `apps/desktop/src/icons.ts` creates offline SVGs without a
framework integration package. The MIT license is included in the built app.

Use 16px icons inheriting the label color. Add them only where they clarify an
action: Add projects, Settings, and Open in Terminal. Existing chevrons, drag
handles, and close controls use the same set. Keep labels on primary actions;
decorative SVGs are hidden from assistive technology. Icon-only buttons retain
accessible names. Task status symbols keep their distinct shapes and semantic
colors. Do not add an icon to every task title, setting, or explanatory sentence.

## macOS app icon

The header uses the supplied Hyras SVG at
`apps/desktop/public/brand/hyras-logo.svg` beside the black “Prodex” wordmark.
The supplied PNG is preserved alongside it. The header image is decorative because
the wordmark provides the accessible app name. The Dock uses the same SVG paths in
white, centered on an inset rounded square in the app's blue accent. The generator
reads `--accent` from the stylesheet so the tile matches the UI. Original assets
remain unchanged; the white treatment is applied only in the generated Dock SVG.

Run `npm run icons` from `apps/desktop` to regenerate the SVG tile composition,
PNG sizes, and macOS ICNS. The tile uses a 1024px canvas, an 880px rounded square
with 196px corners, and a centered 620px mark. Vector source keeps every size sharp.
Rebuild and relaunch the native app to see the Dock icon; frontend hot reload cannot
update it.


## Settings layout

Keep Workload, Planning, and Approvals on one page. Use 14px for section headings,
labels, hints, and the autosave note; distinguish headings with weight 600 and
separate groups with 24px spacing. Keep only concise hints that clarify a setting.
Do not add section introductions, repeated approval callouts, disabled policy
controls, a sidebar, or account/profile placeholders. Account/profile can become
a real section when implemented. Content scrolls within the Settings pane if needed.
The global activation footer remains removed; approvals and task-specific actions
are the desktop's execution controls.
