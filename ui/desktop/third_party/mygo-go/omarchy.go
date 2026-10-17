package design

import (
	"encoding/json"
	"fmt"
	"io"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

// Omarchy consumes bounded flat string TOML data; no file paths, scripts or I/O beyond Reader.
func Omarchy(reader io.Reader) (Scheme, error) {
	raw, err := io.ReadAll(io.LimitReader(reader, 65537))
	if err != nil || len(raw) > 65536 {
		return Scheme{}, fmt.Errorf("Omarchy data exceeds 64 KiB or cannot be read")
	}
	var colors map[string]string
	if err := toml.Unmarshal(raw, &colors); err != nil {
		return Scheme{}, fmt.Errorf("expected flat TOML string data")
	}
	return OmarchyColors(colors)
}

func OmarchyColors(colors map[string]string) (Scheme, error) {
	mode := colors["mode"]
	if mode == "" {
		mode = colors["theme_type"]
	}
	if mode == "" {
		mode = "dark"
	}
	if mode != "dark" && mode != "light" {
		return Scheme{}, fmt.Errorf("invalid Omarchy mode")
	}
	base, err := Builtin("oqto-" + mode)
	if err != nil {
		return Scheme{}, err
	}
	var mapping struct{ Canonical, Legacy map[string]string }
	raw, err := data.ReadFile("data/omarchy.json")
	if err != nil {
		return Scheme{}, err
	}
	if err := json.Unmarshal(raw, &mapping); err != nil {
		return Scheme{}, err
	}
	selected := mapping.Legacy
	if _, ok := colors["lighter_background"]; ok {
		selected = mapping.Canonical
	}
	if _, ok := colors["darker_background"]; ok {
		selected = mapping.Canonical
	}
	for slot, key := range selected {
		value, ok := colors[key]
		if !ok {
			return Scheme{}, fmt.Errorf("missing Omarchy palette key %s", key)
		}
		if len(value) != 7 || !strings.HasPrefix(value, "#") {
			return Scheme{}, fmt.Errorf("Omarchy palette requires six-digit hex")
		}
		if _, err := parseColor(value); err != nil {
			return Scheme{}, err
		}
		base.Slots[slot] = value
	}
	base.ID = "omarchy-data"
	base.Name = "Omarchy data"
	base.Overrides = nil
	return base, nil
}
