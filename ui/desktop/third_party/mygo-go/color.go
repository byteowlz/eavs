package design

import (
	"fmt"
	"math"
	"strconv"
	"strings"

	"github.com/egoist/mygo/ui"
)

// color is unquantized sRGB. Studio's f32/clipping policy is retained until emission.
type color struct{ r, g, b, a float32 }

func (c color) hex() string {
	byteOf := func(v float32) uint8 { return uint8(float32(math.Floor(float64(max(0, min(v, 1))*255) + 0.5))) }
	if c.a >= 0.999 {
		return fmt.Sprintf("#%02x%02x%02x", byteOf(c.r), byteOf(c.g), byteOf(c.b))
	}
	return fmt.Sprintf("#%02x%02x%02x%02x", byteOf(c.r), byteOf(c.g), byteOf(c.b), byteOf(c.a))
}
func (c color) native() ui.Color { return ui.Hex(c.hex()) }
func (c color) mix(o color, t float32) color {
	lerp := func(x, y float32) float32 { return x + (y-x)*t }
	return color{lerp(c.r, o.r), lerp(c.g, o.g), lerp(c.b, o.b), lerp(c.a, o.a)}
}

func number(s string, scale float32) (float32, error) {
	if strings.HasSuffix(s, "%") {
		s = strings.TrimSuffix(s, "%")
		scale = 100
	}
	value, err := strconv.ParseFloat(s, 32)
	if err != nil || math.IsNaN(value) || math.IsInf(value, 0) {
		return 0, fmt.Errorf("invalid finite color component")
	}
	return float32(value) / scale, nil
}

func parseColor(value string) (color, error) {
	s := strings.TrimSpace(value)
	if strings.HasPrefix(s, "#") {
		return parseHex(s[1:])
	}
	if strings.HasPrefix(s, "oklch(") && strings.HasSuffix(s, ")") {
		return parseOklch(s[6 : len(s)-1])
	}
	for _, prefix := range []string{"rgba(", "rgb("} {
		if strings.HasPrefix(s, prefix) && strings.HasSuffix(s, ")") {
			return parseRGB(s[len(prefix) : len(s)-1])
		}
	}
	return color{}, fmt.Errorf("unsupported color syntax")
}

func parseHex(h string) (color, error) {
	if len(h) == 3 {
		h = string([]byte{h[0], h[0], h[1], h[1], h[2], h[2]})
	}
	if len(h) != 6 && len(h) != 8 {
		return color{}, fmt.Errorf("expected 3, 6 or 8 hex digits")
	}
	channels := [4]float32{0, 0, 0, 1}
	for i := 0; i < len(h)/2; i++ {
		n, err := strconv.ParseUint(h[i*2:i*2+2], 16, 8)
		if err != nil {
			return color{}, fmt.Errorf("invalid hex color")
		}
		channels[i] = float32(n) / 255
	}
	return color{channels[0], channels[1], channels[2], channels[3]}, nil
}

func parseRGB(body string) (color, error) {
	parts := strings.Fields(strings.NewReplacer(",", " ", "/", " ").Replace(body))
	if len(parts) != 3 && len(parts) != 4 {
		return color{}, fmt.Errorf("expected RGB and optional alpha")
	}
	channels := [4]float32{0, 0, 0, 1}
	for i, part := range parts {
		scale := float32(255)
		if i == 3 {
			scale = 1
		}
		value, err := number(part, scale)
		if err != nil || value < 0 || value > 1 {
			return color{}, fmt.Errorf("RGB/alpha out of range")
		}
		channels[i] = value
	}
	return color{channels[0], channels[1], channels[2], channels[3]}, nil
}

func parseOklch(body string) (color, error) {
	lch, alpha, hasAlpha := strings.Cut(body, "/")
	parts := strings.Fields(lch)
	if len(parts) != 3 {
		return color{}, fmt.Errorf("expected L C H")
	}
	l, err := number(parts[0], 1)
	if err != nil || l < 0 || l > 1 {
		return color{}, fmt.Errorf("invalid Oklch L")
	}
	c, err := number(parts[1], 1)
	if err != nil || c < 0 || c > 1 {
		return color{}, fmt.Errorf("invalid Oklch C")
	}
	h, err := number(strings.TrimSuffix(parts[2], "deg"), 1)
	if err != nil {
		return color{}, err
	}
	a := float32(1)
	if hasAlpha {
		a, err = number(strings.TrimSpace(alpha), 1)
		if err != nil || a < 0 || a > 1 {
			return color{}, fmt.Errorf("invalid alpha")
		}
	}
	return fromOklch(l, c, h, a), nil
}

// Port of Design Studio studio/color.py: clipped sRGB, not MyGo wide-gamut remapping.
func fromOklch(l, c, h, alpha float32) color {
	h = h * (float32(math.Pi) / 180)
	a := c * float32(math.Cos(float64(h)))
	b := c * float32(math.Sin(float64(h)))
	ll := l + float32(0.39633778)*a + float32(0.21580376)*b
	mm := l - float32(0.105561346)*a - float32(0.06385417)*b
	ss := l - float32(0.08948418)*a - float32(1.2914855)*b
	l3, m3, s3 := ll*ll*ll, mm*mm*mm, ss*ss*ss
	r := float32(4.0767417)*l3 - float32(3.3077116)*m3 + float32(0.23096994)*s3
	g := float32(-1.268438)*l3 + float32(2.6097574)*m3 - float32(0.34131938)*s3
	blue := float32(-0.004196086)*l3 - float32(0.7034186)*m3 + float32(1.7076147)*s3
	return color{gamma(r), gamma(g), gamma(blue), alpha}
}
func gamma(v float32) float32 {
	v = max(0, min(v, 1))
	if v <= float32(0.0031308) {
		return float32(12.92) * v
	}
	return float32(1.055)*float32(math.Pow(float64(v), float64(float32(1)/float32(2.4)))) - float32(0.055)
}
