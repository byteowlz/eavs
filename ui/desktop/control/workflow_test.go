package control

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func testChange() Change {
	return Change{SchemaVersion: 1, Revision: strings.Repeat("a", 64), CandidateRevision: strings.Repeat("b", 64)}
}
func TestWorkflowRequiresExplicitTrustReferencesAndTargets(t *testing.T) {
	draft := Enrollment{Name: "home", URL: "https://eavs.example.test", KeyEnv: "EAVS_KEY", Trust: true}
	if !draft.Valid() {
		t.Fatal("valid reference draft rejected")
	}
	for _, change := range []Enrollment{{Name: "home", URL: draft.URL, KeyEnv: "eavs-private-key", Trust: true}, {Name: "home", URL: "https://user:secret@example.test", KeyEnv: "KEY", Trust: true}, {Name: "home", URL: "https://example.test/admin", KeyEnv: "KEY", Trust: true}, {Name: "home", URL: "http://127.0.0.1:8080", KeyEnv: "KEY", Trust: true}, {Name: "home", URL: "http://localhost:8080", KeyEnv: "KEY", Trust: true, AllowLoopbackHTTP: true}, {Name: "home", URL: draft.URL, KeyEnv: "KEY"}} {
		if change.Valid() {
			t.Fatal("unsafe enrollment accepted")
		}
	}
	draft.URL = "http://127.0.0.1:8080"
	draft.AllowLoopbackHTTP = true
	if !draft.Valid() {
		t.Fatal("explicit literal loopback denied")
	}
	if (PiTarget{Name: "home", Output: "models.json"}).Valid() || (PiTarget{Name: "home", Output: filepath.Join(t.TempDir(), "auth.json")}).Valid() {
		t.Fatal("implicit/wrong target accepted")
	}
}
func TestWorkflowUsesPinnedArgumentsAndNeverSecretValues(t *testing.T) {
	plan := testChange()
	body, err := json.Marshal(plan)
	if err != nil {
		t.Fatal(err)
	}
	output := filepath.Join(t.TempDir(), "arguments")
	path := executable(t, "printf '%s\\n' \"$@\" > '"+output+"'\nprintf '%s' '"+string(body)+"'\n")
	runner := ExecRunner{Binary: path}
	draft := Enrollment{Name: "home", URL: "https://eavs.example.test", KeyEnv: "EAVS_KEY", ListenerKeyEnv: "EAVS_LISTENER", Trust: true}
	if _, err := runner.Enrollment(context.Background(), draft, nil); err != nil {
		t.Fatal(err)
	}
	arguments, err := os.ReadFile(output)
	if err != nil {
		t.Fatal(err)
	}
	if string(arguments) != "device\nconnect\n--name\nhome\n--url\nhttps://eavs.example.test\n--key-env\nEAVS_KEY\n--trust\n--listener-key-env\nEAVS_LISTENER\n" {
		t.Fatal("unexpected command arguments", string(arguments))
	}
	applied := plan
	applied.Applied = true
	body, err = json.Marshal(applied)
	if err != nil {
		t.Fatal(err)
	}
	runner.Binary = executable(t, "printf '%s\\n' \"$@\" > '"+output+"'\nprintf '%s' '"+string(body)+"'\n")
	if _, err := runner.Pi(context.Background(), PiTarget{Name: "home", Output: filepath.Join(t.TempDir(), "models.json")}, &plan); err != nil {
		t.Fatal(err)
	}
	arguments, err = os.ReadFile(output)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasSuffix(string(arguments), "--expected-revision\n"+plan.Revision+"\n--candidate-revision\n"+plan.CandidateRevision+"\n--yes\n") {
		t.Fatal("pins not passed")
	}
}
func TestWorkflowRefusesUnapprovedOrMalformedReplies(t *testing.T) {
	runner := ExecRunner{Binary: executable(t, "echo private-secret >&2; exit 1")}
	target := PiTarget{Name: "home", Output: filepath.Join(t.TempDir(), "models.json")}
	if _, err := runner.Pi(context.Background(), target, nil); err == nil || strings.Contains(err.Error(), "private-secret") {
		t.Fatal("private error escaped")
	}
	bad := testChange()
	bad.Revision = "bad"
	if _, err := runner.Pi(context.Background(), target, &bad); err != ErrInvalid {
		t.Fatal("bad approval accepted")
	}
	body, err := json.Marshal(testChange())
	if err != nil {
		t.Fatal(err)
	}
	runner.Binary = executable(t, "printf '%s' '"+string(body)+"'")
	approved := testChange()
	if _, err := runner.Pi(context.Background(), target, &approved); err != ErrInvalid {
		t.Fatal("unapplied reply acknowledged as write")
	}
}
