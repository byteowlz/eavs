module github.com/byteowlz/eavs/desktop

go 1.27.1

require (
	github.com/byteowlz/design-system/impls/mygo-go v0.0.0
	github.com/byteowlz/eavs/desktop/control v0.0.0
	github.com/egoist/mygo v0.3.2
)

require (
	github.com/ebitengine/purego v0.11.1 // indirect
	github.com/go-text/typesetting v0.3.5 // indirect
	github.com/pelletier/go-toml/v2 v2.2.4 // indirect
	golang.org/x/image v0.46.0 // indirect
)

replace github.com/byteowlz/design-system/impls/mygo-go => ./third_party/mygo-go

replace github.com/byteowlz/eavs/desktop/control => ./control
