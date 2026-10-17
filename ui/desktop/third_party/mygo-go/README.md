# MyGo native adapter

Canonical byteowlz Go implementation for MyGo v0.3.2 native `ui.View` consumers.
No HTML, JavaScript, webview or frontend build. `Resolve(scheme, radius)` returns
all 16 closed roles, normalized 24-slot sRGB output, the R1 scale and a complete
`ui.Theme`. No `DarkTheme`/`LightTheme` palette is copied.

```go
scheme, err := design.Builtin("oqto-dark") // handle errors
resolved, err := design.Resolve(scheme, 0)
c.SetTheme(&resolved.Theme)
ui.Text(c, "Example").TextColor(resolved.Roles[design.Foreground])
```

`ReadScheme` accepts a bounded scheme JSON reader. `Omarchy` accepts bounded,
flat string TOML, never paths/scripts/execution. Canonical palette fills 24
reference keys; legacy ANSI fills all 16 keys and leaves unspecified slots
at the chosen house scheme. This is explicitly a partial legacy overlay.
Malformed/incomplete input fails before application.

`just check`: format, vet, race tests and real Go build.
Tests resolve every copied Studio scheme and Base16 Nord against observed
Studio sRGB outputs, all closed roles/native field bindings, R1 at 0/4/8/32,
pure Base16 aliases, invalid colors and Omarchy no-execution input.

## Native vocabulary and limits

Portable role membership/binding is unchanged source data in data/roles.json,
extracted from shadcn-ts. Native `ui.Theme` fields are framework vocabulary:

- background/text/text-muted/border = matching roles;
- surface = surface; hover = accent; pressed = secondary;
- accent and its hover/pressed = primary (no invented hue shifts);
- accent text = background, matching canonical primary-foreground;
- selection = accent; focus = ring; scrollbar = muted-foreground;
- inverse = surface-sunken, inverse text = foreground;
- danger/warning/success = their portable roles.

Controls use R1 SM; consumer surfaces choose MD/LG/XL from Radius.
Spacing 4, font size 14, scrollbar width 6 are explicit native metrics, not
color decisions. Font family is the system's unless a consumer sets identity.
The toolkit shares a Surface field for inputs/buttons; distinct input overrides
cannot be independently applied through that field. The Input role remains
available for custom native controls. Info remains available for native status.
Tool-local chart/code/etc overrides are not interpreted by this adapter.
Closed-role overrides are supported only with the accepted color grammar.

Color grammar: #rgb/#rrggbb/#rrggbbaa, rgb()/rgba(), oklch(L C H [/ A]).
Float32 color conversion, clipping and byte emission deliberately follow
Studio's sRGB reference, not MyGo's wide-gamut remapping. Wide-P3 parity is
not claimed. Unsupported CSS expressions/named colors fail, not approximate.
Base16 widening uses the canonical 18/34% darkening and 22% brightening,
before quantization, and the opt-in pureBase16 alias policy.
No additional slots/roles are introduced.

Mechanism reuse is not app composition approval. Actual native render,
accessibility/platform and hardware claims belong to consumers' own evidence.
See [PROVENANCE.md](PROVENANCE.md).
