package main

import "fmt"
import (
	"net/http"

	m "github.com/acme/lib/sub"
	_ "embed"
	. "example.com/app/pkg/util"
	"example.com/app"
	"example.com/app/internal/store"
	"example.com/app/internal/missing"
	"example.com/app/internal/onlytests"
	"example.com/app/docs"
	"example.com/shared"
	"example.com/legacy/x"
	"example.com/gone/y"
	"github.com/old/thing/z"
	"golang.org/x/sync/errgroup"
	"go.uber.org/zap/zapcore"
	"github.com/other/thing/v2/deep"
	"github.com/vend/dep/sub"
	"C"
)

func main() {
	fmt.Println(http.StatusOK, m.X, Upper("x"), app.Name(), store.Open(), shared.V, x.V, errgroup.Group{}, zapcore.Level(0), deep.V, sub.V)
}
