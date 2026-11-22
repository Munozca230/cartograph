// Package app is the module's root package.
package app

import "example.com/app/pkg/util"

func Name() string { return util.Upper("app") }
