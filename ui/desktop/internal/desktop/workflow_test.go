package desktop

import (
	"context"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/byteowlz/eavs/desktop/control"
	"github.com/egoist/mygo/ui"
)

type fakeWorkflow struct {
	Calls   atomic.Int32
	Applies atomic.Int32
	Fail    atomic.Bool
}

func (w *fakeWorkflow) change(approved *control.Change) (control.Change, error) {
	w.Calls.Add(1)
	if w.Fail.Load() {
		return control.Change{}, control.ErrUnavailable
	}
	result := control.Change{SchemaVersion: 1, Revision: strings.Repeat("a", 64), CandidateRevision: strings.Repeat("b", 64), ManagedProviders: []string{"eavs-device-home.alpha"}}
	if approved != nil {
		w.Applies.Add(1)
		result.Applied = true
	}
	return result, nil
}
func (w *fakeWorkflow) Enrollment(_ context.Context, _ control.Enrollment, approved *control.Change) (control.Change, error) {
	return w.change(approved)
}
func (w *fakeWorkflow) Pi(_ context.Context, _ control.PiTarget, approved *control.Change) (control.Change, error) {
	return w.change(approved)
}
func TestNativeEnrollmentRequiresTrustPreviewAndUnchangedApproval(t *testing.T) {
	app, _, callbacks := makeApp(t)
	defer app.Close()
	writer := &fakeWorkflow{}
	app.Writer = writer
	tester := ui.NewTester(app.View, 1120, 900)
	if err := tester.Click("Connect device"); err != nil {
		t.Fatal(err)
	}
	app.Draft = control.Enrollment{Name: "home", URL: "https://eavs.example.test", KeyEnv: "EAVS_KEY"}
	tester.Frame()
	if err := tester.Click("Preview change"); err == nil && writer.Calls.Load() != 0 {
		t.Fatal("untrusted preview executed")
	}
	app.beginWorkflow(true)
	if writer.Calls.Load() != 0 {
		t.Fatal("apply without preview")
	}
	app.Draft.Trust = true
	tester.Frame()
	if err := tester.Click("Preview change"); err != nil {
		t.Fatal(err)
	}
	dispatch(t, callbacks)
	tester.Frame()
	if app.Pending == nil || writer.Calls.Load() != 1 || writer.Applies.Load() != 0 {
		t.Fatal("preview not isolated")
	}
	app.Draft.Name = "changed"
	app.beginWorkflow(true)
	if writer.Calls.Load() != 1 {
		t.Fatal("modified approval reused")
	}
	app.Draft.Name = "home"
	tester.Frame()
	if err := tester.Click("Confirm and apply"); err != nil {
		t.Fatal(err)
	}
	dispatch(t, callbacks)
	tester.Frame()
	if writer.Applies.Load() != 1 || app.Pending != nil || !tester.HasText("Applied. Refresh connections or reopen Pi’s model picker to inspect the result.") {
		t.Fatal("apply receipt not acknowledged")
	}
	tester.SetSize(640, 900)
	if !tester.HasText("Back / cancel") {
		t.Fatal("narrow layout lost cancellation")
	}
}
func TestNativePiRequiresExplicitTargetAndFailedChangesCannotBeApplied(t *testing.T) {
	app, runner, callbacks := makeApp(t)
	defer app.Close()
	writer := &fakeWorkflow{}
	app.Writer = writer
	app.Refresh()
	dispatch(t, callbacks)
	app.Model.Select("home")
	tester := ui.NewTester(app.View, 1120, 900)
	if err := tester.Click("Connect Pi"); err != nil {
		t.Fatal(err)
	}
	app.beginWorkflow(false)
	if writer.Calls.Load() != 0 {
		t.Fatal("implicit file target executed")
	}
	app.PiTarget.Output = "/tmp/eavs-fixture/models.json"
	tester.Frame()
	if err := tester.Click("Preview change"); err != nil {
		t.Fatal(err)
	}
	dispatch(t, callbacks)
	tester.Frame()
	if app.Pending == nil || runner.Calls.Load() != 1 || writer.Calls.Load() != 1 {
		t.Fatal("Pi preview performed unrelated work")
	}
	writer.Fail.Store(true)
	if err := tester.Click("Confirm and apply"); err != nil {
		t.Fatal(err)
	}
	dispatch(t, callbacks)
	tester.Frame()
	if app.Pending != nil || writer.Calls.Load() != 2 || strings.Contains(strings.Join(tester.Texts(), " "), "private-secret") {
		t.Fatal("failed change retained approval or leaked data")
	}
	app.beginWorkflow(true)
	if writer.Calls.Load() != 2 {
		t.Fatal("failed apply retried automatically")
	}
	app.dismissWorkflow()
	app.Close()
	app.Panel = "pi"
	app.beginWorkflow(false)
	if writer.Calls.Load() != 2 {
		t.Fatal("closed app started write")
	}
}
