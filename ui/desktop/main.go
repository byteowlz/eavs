package main

import (
	"flag"
	"fmt"
	"os"

	design "github.com/byteowlz/design-system/impls/mygo-go"
	"github.com/byteowlz/eavs/desktop/control"
	"github.com/byteowlz/eavs/desktop/internal/desktop"
	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
)

func main() {
	binary := flag.String("eavs-binary", "eavs", "Explicit eavs executable; invoked without a shell")
	schemeName := flag.String("scheme", "oqto-dark", "Canonical built-in design-system scheme")
	flag.Parse()
	if flag.NArg() != 0 {
		fmt.Fprintln(os.Stderr, "unexpected arguments; use --help")
		os.Exit(2)
	}
	scheme, err := design.Builtin(*schemeName)
	if err != nil {
		fmt.Fprintln(os.Stderr, "invalid design-system scheme")
		os.Exit(2)
	}
	theme, err := design.Resolve(scheme, 8)
	if err != nil {
		fmt.Fprintln(os.Stderr, "design-system resolution failed")
		os.Exit(2)
	}
	mygo.App.WhenReady(func() {
		app := desktop.New(control.ExecRunner{Binary: *binary}, theme)
		window := mygo.NewWindow(mygo.WindowOptions{Title: "Eavs — Connection Desk", Width: 1120, Height: 760, MinWidth: 640, MinHeight: 520, Content: ui.View(app.View)})
		app.Update = window.Update
		window.OnClosed(app.Close)
	})
	if err := mygo.App.Run(); err != nil {
		fmt.Fprintln(os.Stderr, "native window runtime failed")
		os.Exit(1)
	}
}
