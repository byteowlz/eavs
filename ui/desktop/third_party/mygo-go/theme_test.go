package design

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"math"
	"os"
	"reflect"
	"regexp"
	"strings"
	"testing"

	"github.com/egoist/mygo/ui"
)

func TestCanonicalStudioParity(t *testing.T) {
	raw, err := os.ReadFile("testdata/parity.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixtures map[string]struct{ Slots, Roles map[string]string }
	if err := json.Unmarshal(raw, &fixtures); err != nil {
		t.Fatal(err)
	}
	for id, expected := range fixtures {
		t.Run(id, func(t *testing.T) {
			scheme, err := Builtin(id)
			if err != nil {
				t.Fatal(err)
			}
			resolved, err := Resolve(scheme, 8)
			if err != nil {
				t.Fatal(err)
			}
			if !reflect.DeepEqual(resolved.Slots, expected.Slots) {
				t.Fatalf("slot parity: %+v expected %+v", resolved.Slots, expected.Slots)
			}
			if len(resolved.Roles) != 16 {
				t.Fatal("roles not closed")
			}
			for role, value := range expected.Roles {
				if got := resolved.Roles[Role(role)]; got != ui.Hex(value) {
					t.Fatalf("%s: %+v != %s", role, got, value)
				}
			}
			r := resolved.Roles
			theme := resolved.Theme
			if theme.Background != r[Background] || theme.Surface != r[Surface] || theme.Text != r[Foreground] ||
				theme.TextMuted != r[MutedForeground] || theme.Accent != r[Primary] || theme.AccentText != r[Background] ||
				theme.Focus != r[Ring] || theme.Selection != r[Accent] || theme.Inverse != r[SurfaceSunken] ||
				theme.Danger != r[Danger] || theme.Success != r[Success] || theme.Warning != r[Warning] ||
				theme.Border != r[Border] || theme.Scrollbar != r[MutedForeground] ||
				theme.SurfaceHover != r[Accent] || theme.SurfacePressed != r[Secondary] ||
				theme.AccentHover != r[Primary] || theme.AccentPressed != r[Primary] ||
				theme.InverseText != r[Foreground] {
				t.Fatal("native vocabulary role parity")
			}
			if theme.Radius != resolved.Radius.SM {
				t.Fatal("control radius is not SM")
			}
		})
	}
}

func TestRadiusAndBase16Alias(t *testing.T) {
	for _, dial := range []float32{0, 4, 8, 32, math.MaxFloat32 / 2} {
		got, err := Radius(dial)
		if err != nil {
			t.Fatal(err)
		}
		if got != (RadiusScale{dial * 0.5, dial * 0.75, dial, dial * 1.25}) {
			t.Fatal(got)
		}
	}
	for _, dial := range []float32{-1, float32(math.NaN()), float32(math.Inf(1)), math.MaxFloat32} {
		if _, err := Radius(dial); err == nil {
			t.Fatal("accepted invalid radius")
		}
	}
	scheme, _ := Builtin("nord")
	scheme.PureBase16 = true
	resolved, err := Resolve(scheme, 0)
	if err != nil {
		t.Fatal(err)
	}
	if resolved.Slots["base10"] != resolved.Slots["base00"] || resolved.Slots["base12"] != resolved.Slots["base08"] {
		t.Fatal("pure Base16 alias mismatch")
	}
	delete(scheme.Slots, "base05")
	if _, err := Resolve(scheme, 0); err == nil {
		t.Fatal("missing slot accepted")
	}
}

func TestDataOnlyOmarchy(t *testing.T) {
	base, _ := Builtin("oqto-dark")
	var m struct{ Canonical, Legacy map[string]string }
	raw, _ := data.ReadFile("data/omarchy.json")
	json.Unmarshal(raw, &m)
	resolved, _ := Resolve(base, 0)
	for _, mapping := range []map[string]string{m.Canonical, m.Legacy} {
		colors := map[string]string{}
		for slot, key := range mapping {
			colors[key] = resolved.Slots[slot]
		}
		scheme, err := OmarchyColors(colors)
		if err != nil {
			t.Fatal(err)
		}
		actual, err := Resolve(scheme, 0)
		if err != nil {
			t.Fatal(err)
		}
		for slot := range mapping {
			if actual.Slots[slot] != resolved.Slots[slot] {
				t.Fatal("slot mapping drift")
			}
		}
	}
	for _, text := range []string{"bad=[", "background=123", "color0='#123456'", strings.Repeat("x", 65537)} {
		if _, err := Omarchy(strings.NewReader(text)); err == nil {
			t.Fatal("accepted incomplete/malformed input")
		}
	}
	if _, err := parseColor("url(https://invalid.test)"); err == nil {
		t.Fatal("accepted executable/CSS syntax")
	}
	for _, value := range []string{"oklch(NaN 0.1 20)", "#xyz", "rgba(0,0,0,2)"} {
		if _, err := parseColor(value); err == nil {
			t.Fatal("accepted invalid color")
		}
	}
}

func TestOmarchyInputTableProvenance(t *testing.T) {
	source, err := os.ReadFile("reference/templates-omarchy.mjs.txt")
	if err != nil {
		t.Fatal(err)
	}
	raw, err := data.ReadFile("data/omarchy.json")
	if err != nil {
		t.Fatal(err)
	}
	var tables map[string]map[string]string
	if err := json.Unmarshal(raw, &tables); err != nil {
		t.Fatal(err)
	}
	for key, name := range map[string]string{"canonical": "CANONICAL", "legacy": "LEGACY_ANSI"} {
		block := strings.Split(strings.Split(string(source), "const "+name+" = {")[1], "};")[0]
		expected := map[string]string{}
		for _, m := range regexp.MustCompile(`(base[0-9A-F]{2}): "([^"]+)"`).FindAllStringSubmatch(block, -1) {
			expected[m[1]] = m[2]
		}
		if !reflect.DeepEqual(tables[key], expected) {
			t.Fatal("Omarchy table differs from archived canonical input map")
		}
	}
}

func TestProvenanceAndRoleTable(t *testing.T) {
	raw, err := os.ReadFile("reference/hashes.json")
	if err != nil {
		t.Fatal(err)
	}
	var hashes map[string]string
	json.Unmarshal(raw, &hashes)
	for path, want := range hashes {
		bytes, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		sum := sha256.Sum256(bytes)
		if hex.EncodeToString(sum[:]) != want {
			t.Fatalf("source drift: %s", path)
		}
	}
	source, _ := os.ReadFile("reference/roles.ts.txt")
	block := strings.Split(strings.Split(string(source), "export const ROLE_FOR_SLOT")[1], "};")[0]
	bindings := map[Role]string{}
	for _, m := range regexp.MustCompile(`([a-z-]+|"[a-z-]+"): "(base[0-9A-F]{2})"`).FindAllStringSubmatch(block, -1) {
		bindings[Role(strings.Trim(m[1], "\""))] = m[2]
	}
	raw, _ = data.ReadFile("data/roles.json")
	var table map[Role]string
	json.Unmarshal(raw, &table)
	if len(table) != 16 || !reflect.DeepEqual(bindings, table) {
		t.Fatal("role table differs from pinned canonical engine")
	}
}
