package control

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"net/url"
	"path/filepath"
	"regexp"
)

// Enrollment contains references, never credential values. The Rust operation
// authoritatively validates trust, live scope and both publication revisions.
type Enrollment struct {
	Name, URL, KeyEnv, ListenerKeyEnv string
	Trust, AllowLoopbackHTTP          bool
}
type PiTarget struct{ Name, Output string }
type Change struct {
	SchemaVersion     int      `json:"schema_version"`
	Applied           bool     `json:"applied"`
	Revision          string   `json:"revision"`
	CandidateRevision string   `json:"candidate_revision"`
	ManagedProviders  []string `json:"managed_providers"`
	SkippedModels     int      `json:"skipped_models"`
}
type Workflow interface {
	Enrollment(context.Context, Enrollment, *Change) (Change, error)
	Pi(context.Context, PiTarget, *Change) (Change, error)
}

var identifierPattern = regexp.MustCompile(`^[A-Za-z0-9_.-]{1,128}$`)
var referencePattern = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]{0,127}$`)

func identifier(value string) bool {
	return identifierPattern.MatchString(value) && value != "." && value != ".."
}
func (draft Enrollment) Valid() bool {
	u, err := url.Parse(draft.URL)
	if err != nil || !endpoint(draft.URL) || (u.Path != "" && u.Path != "/") {
		return false
	}
	secure := u.Scheme == "https" || draft.AllowLoopbackHTTP && (u.Hostname() == "127.0.0.1" || u.Hostname() == "::1")
	return secure && identifier(draft.Name) && referencePattern.MatchString(draft.KeyEnv) && (draft.ListenerKeyEnv == "" || referencePattern.MatchString(draft.ListenerKeyEnv)) && draft.Trust
}
func (target PiTarget) Valid() bool {
	return identifier(target.Name) && filepath.IsAbs(target.Output) && filepath.Base(target.Output) == "models.json" && clean(target.Output, 4096)
}
func pin(value string) bool {
	bytes, err := hex.DecodeString(value)
	return err == nil && len(bytes) == 32 && len(value) == 64
}
func (runner ExecRunner) change(ctx context.Context, args []string, approved *Change) (Change, error) {
	if approved != nil {
		if approved.SchemaVersion != 1 || approved.Applied || !pin(approved.Revision) || !pin(approved.CandidateRevision) {
			return Change{}, ErrInvalid
		}
		args = append(args, "--expected-revision", approved.Revision, "--candidate-revision", approved.CandidateRevision, "--yes")
	}
	output, err := runner.invoke(ctx, args)
	if err != nil {
		return Change{}, err
	}
	var result Change
	if json.Unmarshal(output, &result) != nil || result.SchemaVersion != 1 || result.Applied != (approved != nil) || !pin(result.Revision) || !pin(result.CandidateRevision) || len(result.ManagedProviders) > 256 || result.SkippedModels < 0 || result.SkippedModels > 4096 {
		return Change{}, ErrInvalid
	}
	if approved != nil && (result.Revision != approved.Revision || result.CandidateRevision != approved.CandidateRevision) {
		return Change{}, ErrInvalid
	}
	for _, name := range result.ManagedProviders {
		if !clean(name, 512) {
			return Change{}, ErrInvalid
		}
	}
	return result, nil
}
func (runner ExecRunner) Enrollment(ctx context.Context, draft Enrollment, approved *Change) (Change, error) {
	if !draft.Valid() {
		return Change{}, ErrInvalid
	}
	args := []string{"device", "connect", "--name", draft.Name, "--url", draft.URL, "--key-env", draft.KeyEnv, "--trust"}
	if draft.ListenerKeyEnv != "" {
		args = append(args, "--listener-key-env", draft.ListenerKeyEnv)
	}
	if draft.AllowLoopbackHTTP {
		args = append(args, "--allow-loopback-http")
	}
	return runner.change(ctx, args, approved)
}
func (runner ExecRunner) Pi(ctx context.Context, target PiTarget, approved *Change) (Change, error) {
	if !target.Valid() {
		return Change{}, ErrInvalid
	}
	return runner.change(ctx, []string{"device", "pi", "--name", target.Name, "--output", target.Output}, approved)
}
