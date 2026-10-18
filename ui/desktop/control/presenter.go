package control

// Presenter is UI-thread-owned. Worker results carry generations; stale results
// and results delivered after Close cannot change the visible model.
type Presenter struct {
	Snapshot   Snapshot
	Selected   string
	Busy       bool
	Stale      bool
	Message    string
	generation uint64
	closed     bool
}

func NewPresenter() Presenter {
	return Presenter{Message: "No connection data loaded. Refresh uses the shared eavs CLI; no inference is generated.", Stale: true}
}
func (p *Presenter) Begin() uint64 {
	if p.closed {
		return 0
	}
	p.generation++
	p.Busy = true
	p.Message = "Reading device connection metadata…"
	return p.generation
}
func cloneSnapshot(value Snapshot) Snapshot {
	copy := Snapshot{SchemaVersion: value.SchemaVersion, Connections: make([]Connection, len(value.Connections))}
	for i, connection := range value.Connections {
		connection.Providers = append([]Provider(nil), connection.Providers...)
		for j, provider := range connection.Providers {
			provider.Models = append([]Model(nil), provider.Models...)
			connection.Providers[j] = provider
		}
		copy.Connections[i] = connection
	}
	return copy
}
func (p *Presenter) Complete(generation uint64, snapshot Snapshot, err error) bool {
	if p.closed || generation == 0 || generation != p.generation {
		return false
	}
	p.Busy = false
	if err != nil {
		p.Stale = true
		p.Message = "Refresh failed. Last-known data is stale; check eavs device status --json."
		return true
	}
	p.Snapshot = cloneSnapshot(snapshot)
	p.Stale = false
	p.Message = "Connection metadata refreshed. Model candidates do not prove upstream health or available quota."
	if p.Selected != "" && p.Connection() == nil {
		p.Selected = ""
	}
	return true
}
func (p *Presenter) Select(name string) bool {
	if p.closed {
		return false
	}
	for _, connection := range p.Snapshot.Connections {
		if connection.Name == name {
			p.Selected = name
			return true
		}
	}
	return false
}
func (p *Presenter) Connection() *Connection {
	for _, connection := range p.Snapshot.Connections {
		if connection.Name == p.Selected {
			copy := cloneSnapshot(Snapshot{Connections: []Connection{connection}})
			return &copy.Connections[0]
		}
	}
	return nil
}
func (p *Presenter) Close() { p.closed = true; p.generation++; p.Busy = false }
