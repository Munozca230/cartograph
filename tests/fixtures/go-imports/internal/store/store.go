package store

import (
	"database/sql"

	"../../pkg/util"
)

func Open() *sql.DB { _ = util.Upper(""); return nil }
