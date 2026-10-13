# Notebook

A notes app in Rust on egui and [Patina](https://github.com/Sector-F-Labs/patina): open a folder of
markdown files and edit them as rich text, Apple Notes style, with the markdown generated on save.
A rewrite of the GTK Notebook in `com/divanv/notebook-old-gtk`.

Significant, hard-to-reverse architecture decisions are recorded as ADRs in `docs/adr/` (see
`docs/adr/README.md` for the convention, and the `adr` skill for creating one). Read the relevant
ADRs before changing how notes are edited, saved or stored.

## Commands

```bash
make test          # unit tests, including editor tests driven through egui events
make check         # tests + clippy -D warnings
make run           # run on a copy of examples/notes (target/demo-notes)
make screenshot    # docs/screenshot-*.png, in a throwaway config dir
make icons         # assets/notebook-icon-128.png from packaging/linux/notebook.svg (via resvg)
make install       # per-user install to ~/.local (ADR-0008); test with PREFIX=<scratch dir>
```

The user may be running Notebook (this one or the GTK one) while you work. Never touch their
config: run test instances with `XDG_CONFIG_HOME` and `XDG_DATA_HOME` pointing at a scratch
directory (as `make screenshot` does), and never against their own notes folder.

On Wayland, `with_icon` does nothing. GNOME takes the window icon from the installed
`com.sectorflabs.Notebook.desktop`, matched by app id. So "no icon when I cargo run" means the
launcher isn't installed, or the window was opened before it was.

## Layout

- `src/markdown.rs`: markdown ↔ line model (`Line`/`Block`/`Span`), carried over from the GTK
  version unchanged, with round-trip tests.
- `src/doc.rs`: paragraphs (`Para`: text, table, code, image) and span operations, counted in
  characters.
- `src/editor.rs`: the WYSIWYG widget: layout, cursor, selection, keys, mouse, IME, clipboard,
  undo (ADR-0002).
- `src/embeds.rs`: table, code block and image widgets (ADR-0012).
- `src/fonts.rs`: Ubuntu variable fonts for bold and italic note text (ADR-0003).
- `src/notes.rs`: workspace scanning and file helpers, including image paths.
- `src/app.rs`: window, Patina header bar, tabs and toolbar, sidebar, dialogs, autosave, skins.
  `Tab` holds one workspace (folder, tree, open note, editor); `App` holds the tabs and the
  window-wide state (ADR-0011).
- `src/config.rs`: `~/.config/notebook/config.toml`.
- Skins: the Notebook skin lives in Patina (`skins/notebook.toml` there, ADR-0010); this app only
  makes it the starting one (`HOME_SKIN` in `app.rs`). Patina's `TitleBar` colours the bar's
  controls when a skin colours the bar.
