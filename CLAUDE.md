# Notebook

A notes app in Rust on egui and [Patina](https://github.com/Sector-F-Labs/patina): open a folder of
markdown files and edit them as rich text, Apple Notes style, with the markdown generated on save.
A rewrite of the GTK Notebook in `com/divanv/notebook`.

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
- `src/doc.rs`: paragraphs (`Para`: text, table, code) and span operations, counted in characters.
- `src/editor.rs`: the WYSIWYG widget: layout, cursor, selection, keys, mouse, IME, clipboard,
  undo (ADR-0002).
- `src/embeds.rs`: table and code block widgets.
- `src/fonts.rs`: Ubuntu variable fonts for bold and italic note text (ADR-0003).
- `src/notes.rs`: workspace scanning and file helpers.
- `src/app.rs`: window, Patina header bar and toolbar, sidebar, dialogs, autosave, skins.
- `src/config.rs`: `~/.config/notebook/config.toml`.
- `skins/notebook.toml`: the built-in Notebook skin and the starting one (ADR-0009). `bar_controls`
  in `app.rs` colours the title bar's controls when a skin colours the bar; it's a copy of
  Calendar's and belongs in Patina.
