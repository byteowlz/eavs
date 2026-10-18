package desktop

import (
	"context"
	"errors"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	design "github.com/byteowlz/design-system/impls/mygo-go"
	"github.com/byteowlz/eavs/desktop/control"
	"github.com/egoist/mygo/ui"
)

type fakeRunner struct {
	Calls atomic.Int32
	Fail  atomic.Bool
}

func (r *fakeRunner) Status(context.Context) (control.Snapshot, error) {
	r.Calls.Add(1)
	if r.Fail.Load() {
		return control.Snapshot{}, errors.New("private fixture secret")
	}
	return control.Decode([]byte(`{"schema_version":1,"connections":[{"name":"home","url":"https://eavs.example.test","state":"ready","providers":[{"id":"alpha","api":"openai-completions","models":[{"id":"model-a","name":"Model A"}]}]}]}`))
}
func makeApp(t *testing.T) (*App, *fakeRunner, chan func()) {
	t.Helper()
	scheme, err := design.Builtin("oqto-dark")
	if err != nil {
		t.Fatal(err)
	}
	theme, err := design.Resolve(scheme, 8)
	if err != nil {
		t.Fatal(err)
	}
	runner := &fakeRunner{}
	app := New(runner, theme)
	callbacks := make(chan func(), 4)
	app.Update = func(fn func()) { callbacks <- fn }
	return app, runner, callbacks
}
func dispatch(t *testing.T, callbacks chan func()) {
	t.Helper()
	select {
	case fn := <-callbacks:
		fn()
	case <-time.After(time.Second):
		t.Fatal("missing background completion")
	}
}
func TestNativeReadOnlyRefreshAndSelection(t *testing.T) {
	app, runner, callbacks := makeApp(t)
	defer app.Close()
	tester := ui.NewTester(app.View, 1120, 760)
	if runner.Calls.Load() != 0 || !tester.HasText("Choose a connection") {
		t.Fatal("hidden request or selected connection")
	}
	if err := tester.Click("Refresh connections"); err != nil {
		t.Fatal(err)
	}
	dispatch(t, callbacks)
	tester.Frame()
	if runner.Calls.Load() != 1 || app.Model.Selected != "" || !tester.HasText("Metadata reachable") {
		t.Fatal("read result mismatch")
	}
	if err := tester.Click("home"); err != nil {
		t.Fatal(err)
	}
	if !tester.HasText("model-a — Model A") {
		t.Fatal("public model projection missing")
	}
	runner.Fail.Store(true)
	if err := tester.Click("Refresh connections"); err != nil {
		t.Fatal(err)
	}
	dispatch(t, callbacks)
	tester.Frame()
	if !app.Model.Stale || !tester.HasText("LAST KNOWN / NOT CURRENT") || strings.Contains(strings.Join(tester.Texts(), " "), "private fixture secret") {
		t.Fatal("stale/private failure boundary")
	}
	if len(tester.Announcements()) == 0 {
		t.Fatal("status not announced")
	}
	tester.SetSize(640, 760)
	if !tester.HasText("Refresh connections") {
		t.Fatal("narrow layout lost refresh")
	}
}
func TestNativeClosedAppDoesNotStartWork(t *testing.T) {
	app, runner, _ := makeApp(t)
	app.Close()
	app.Refresh()
	if runner.Calls.Load() != 0 || app.Model.Busy {
		t.Fatal("closed app started work")
	}
}
