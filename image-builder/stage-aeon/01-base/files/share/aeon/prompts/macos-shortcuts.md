# macOS keyboard shortcuts cheat sheet

For use with `key_chord` calls. Modifier name in this device's vocabulary
is on the left, the macOS name on the right.

| Our name | macOS modifier |
|---|---|
| `GUI` / `CMD` / `WIN` | Command (⌘) |
| `CTRL` | Control (⌃) |
| `ALT` | Option (⌥) |
| `SHIFT` | Shift (⇧) |

## System navigation
- Spotlight: `["GUI","SPACE"]`
- App switcher next: `["GUI","TAB"]`
- App switcher previous: `["GUI","SHIFT","TAB"]`
- Mission Control: `["CTRL","UP"]`
- Show desktop: `["F11"]` (depends on Mission Control settings)
- Force quit menu: `["GUI","ALT","ESC"]`
- Lock screen: `["CTRL","GUI","Q"]`

## Window management
- Hide app: `["GUI","H"]`
- Hide others: `["GUI","ALT","H"]`
- Minimize: `["GUI","M"]`
- Full-screen toggle: `["CTRL","GUI","F"]`
- Cycle windows of current app: `["GUI","`"]`

## Editing
- Copy: `["GUI","C"]`
- Cut: `["GUI","X"]`
- Paste: `["GUI","V"]`
- Paste-and-match-style: `["GUI","SHIFT","ALT","V"]`
- Undo: `["GUI","Z"]`
- Redo: `["GUI","SHIFT","Z"]`
- Select all: `["GUI","A"]`

## Browser / Safari / Chrome
- New tab: `["GUI","T"]`
- Close tab: `["GUI","W"]`
- Reopen closed tab: `["GUI","SHIFT","T"]`
- Address bar focus: `["GUI","L"]`
- Find on page: `["GUI","F"]`
- Reload: `["GUI","R"]`
- Next tab: `["CTRL","TAB"]`

## Screenshot
- Region picker: `["GUI","SHIFT","4"]` — then user drags
- Full screen: `["GUI","SHIFT","3"]`
- Screenshot tool (with timer/options): `["GUI","SHIFT","5"]`
- Screenshot to clipboard: add `["CTRL"]` to any of the above

## Gotcha
For the multi-touch gestures (3-finger swipe between desktops, 4-finger
Mission Control), you need the `apple-magic` persona loaded
(`set_persona persona="apple-magic"`). The other personas only expose
keyboard + plain 2-button mouse.
