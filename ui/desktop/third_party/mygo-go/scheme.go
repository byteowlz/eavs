// Package design is the canonical byteowlz MyGo native Base24/roles/R1 adapter.
package design

import (
	"embed"
	"encoding/json"
	"fmt"
	"io"
	"strings"
)

//go:embed data/*.json data/schemes/*.json
var data embed.FS

type Scheme struct {
	ID         string            `json:"id"`
	Name       string            `json:"name"`
	Mode       string            `json:"mode"`
	System     string            `json:"system"`
	Slots      map[string]string `json:"slots"`
	Overrides  map[string]string `json:"overrides,omitempty"`
	PureBase16 bool              `json:"pureBase16,omitempty"`
}

func ReadScheme(input io.Reader) (Scheme, error) {
	raw, err := io.ReadAll(io.LimitReader(input, 65537))
	if err != nil || len(raw) > 65536 {
		return Scheme{}, fmt.Errorf("scheme exceeds 64 KiB or cannot be read")
	}
	var scheme Scheme
	decoder := json.NewDecoder(strings.NewReader(string(raw)))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&scheme); err != nil {
		return Scheme{}, err
	}
	if err := decoder.Decode(new(any)); err != io.EOF {
		return Scheme{}, fmt.Errorf("expected exactly one scheme")
	}
	if _, err := normalize(scheme); err != nil {
		return Scheme{}, err
	}
	return scheme, nil
}

func Builtin(id string) (Scheme, error) {
	file, err := data.Open("data/schemes/" + id + ".json")
	if err != nil {
		return Scheme{}, fmt.Errorf("unknown built-in scheme")
	}
	defer file.Close()
	return ReadScheme(file)
}

func normalize(scheme Scheme) (map[string]color, error) {
	if scheme.Mode != "dark" && scheme.Mode != "light" {
		return nil, fmt.Errorf("mode must be dark or light")
	}
	count := 16
	if scheme.System == "base24" {
		count = 24
	} else if scheme.System != "base16" {
		return nil, fmt.Errorf("system must be base16 or base24")
	}
	slots, err := parseSlots(scheme.Slots)
	if err != nil {
		return nil, err
	}
	for i := range count {
		key := fmt.Sprintf("base%02X", i)
		if _, ok := slots[key]; !ok {
			return nil, fmt.Errorf("missing slot %s", key)
		}
	}
	if count == 24 {
		return slots, nil
	}
	widen(slots, scheme.PureBase16)
	return slots, nil
}

func parseSlots(input map[string]string) (map[string]color, error) {
	slots := make(map[string]color, 24)
	for slot, value := range input {
		var index int
		n, err := fmt.Sscanf(slot, "base%02X", &index)
		if err != nil || n != 1 || index < 0 || index >= 24 || slot != fmt.Sprintf("base%02X", index) {
			return nil, fmt.Errorf("unknown slot %q", slot)
		}
		parsed, err := parseColor(value)
		if err != nil {
			return nil, fmt.Errorf("slot %s: %w", slot, err)
		}
		slots[slot] = parsed
	}
	return slots, nil
}

func widen(slots map[string]color, pureBase16 bool) {
	for _, recipe := range []struct {
		target, source string
		amount         float32
		light          bool
	}{
		{"base10", "base00", 0.18, false}, {"base11", "base00", 0.34, false},
		{"base12", "base08", 0.22, true}, {"base13", "base0A", 0.22, true}, {"base14", "base0B", 0.22, true},
		{"base15", "base0C", 0.22, true}, {"base16", "base0D", 0.22, true}, {"base17", "base0E", 0.22, true},
	} {
		if _, ok := slots[recipe.target]; ok {
			continue
		}
		value := slots[recipe.source]
		if !pureBase16 {
			target := color{0, 0, 0, 1}
			if recipe.light {
				target = color{1, 1, 1, 1}
			}
			value = value.mix(target, recipe.amount)
		}
		slots[recipe.target] = value
	}
}
