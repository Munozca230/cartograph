module example.com/app

go 1.22

require (
	github.com/acme/lib v1.2.0
	golang.org/x/sync v0.5.0 // indirect
)

require github.com/vend/dep v1.0.0

replace example.com/legacy => ./third_party/legacy

replace (
	example.com/gone => ../outside
	github.com/old/thing v1.0.0 => github.com/new/thing v1.1.0
)
