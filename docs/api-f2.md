# cartograph serve: contrato de la API HTTP

Este contrato lo comparten el backend (`crates/cartograph-server`) y el frontend (`web/`). Si una de las partes necesita cambiarlo, el cambio se hace aquí primero.

## Reglas generales
- El servidor escucha solo en `127.0.0.1` y nunca en `0.0.0.0`. El puerto por defecto es 7878; si está ocupado, prueba los 10 siguientes y avisa en stderr cuál usó. Con `--port N` explícito y el puerto ocupado, sale con error claro y código distinto de cero.
- Todas las respuestas son JSON (UTF-8) y llevan `"generation": <u64>`, que sube en 1 cada vez que termina un reindexado. El frontend lo usa para invalidar caché.
- Ids de nodo, iguales a los del store: `repo:`, `dir:<ruta>`, `file:<ruta>`, `pkg:<ruta-manifiesto>`, `ext:<npm:react|node:fs|python:os|pypi:requests>`. Las rutas son relativas al repo y usan `/`.
- Errores: HTTP 4xx/5xx con `{ "error": { "code": "not_found" | "bad_request" | "indexing" | "internal", "message": "..." } }`. El servidor nunca entra en pánico por un request.
- Mientras corre el primer indexado, los endpoints de datos devuelven `503` con `code: "indexing"` y `progress`. `/api/meta` y `/api/events` responden siempre.

## GET /api/meta
```json
{
  "generation": 3,
  "status": "ready",
  "progress": null,
  "repo": { "name": "my-app", "root": "/home/me/my-app" },
  "indexed_at": "2026-11-20T06:30:00Z",
  "index_ms": 350,
  "counts": { "files": 120, "dirs": 40, "packages": 1, "loc": 12345,
              "imports": 300, "resolved": 180, "external": 120, "unresolved": 0,
              "skipped": 2, "diagnostics": 0 },
  "languages": [ { "lang": "typescript", "files": 70, "loc": 8000 } ],
  "watching": true
}
```
`status` puede ser `"indexing"` (con `progress: { "done": 80, "total": 120 }`), `"ready"` o `"error"` (con `"error": "..."`).

## GET /api/tree?root=<dir-id>&depth=<n>
Devuelve la jerarquía canónica, que es el árbol de directorios. Los paquetes no forman parte de este árbol: van como overlay en `/api/packages`. Sin parámetros devuelve el árbol completo. `depth` limita la profundidad y los nodos recortados traen `"truncated": true`.
```json
{
  "generation": 3,
  "root": {
    "id": "repo:", "kind": "repo", "name": "my-app", "path": "",
    "loc": 12345, "files": 120, "fan_in": 0, "fan_out": 0, "unresolved": 0,
    "children": [
      { "id": "dir:src", "kind": "dir", "name": "src", "path": "src",
        "loc": 11000, "files": 100, "fan_in": 0, "fan_out": 12, "unresolved": 0,
        "langs": { "typescript": 100, "tsx": 45 },
        "children": [
          { "id": "file:src/lib/db/schema.ts", "kind": "file", "name": "schema.ts",
            "path": "src/lib/db/schema.ts", "lang": "typescript", "loc": 300,
            "fan_in": 40, "fan_out": 2, "unresolved": 0 }
        ] }
    ]
  }
}
```
Métricas de un directorio:
- `files` y `loc` cuentan todos los archivos descendientes.
- `fan_in` y `fan_out` cuentan los archivos distintos de fuera del directorio que importan algo de adentro, o que son importados desde adentro.
- `unresolved` suma los imports no resueltos de los archivos descendientes.

## GET /api/graph?depth=<n|all>&expanded=<dir-ids>&collapsed=<dir-ids>
Grafo agregado al nivel visible en pantalla.
- **Frontera visible**: empieza en los hijos directos de `repo:` (directorios y archivos de primer nivel). Cada directorio abierto se reemplaza por sus hijos, recursivamente. Si se pide expandir un directorio cuyo padre no está expandido, se ignora y se informa en `ignored`.
- **Qué directorios se abren**. Los directorios de primer nivel son el nivel 1, sus hijos el nivel 2, etc. Un directorio está abierto si su padre está abierto, no aparece en `collapsed` y además está por encima del nivel pedido (`nivel < depth`) o aparece en `expanded`.
  - `depth=1` (por defecto) no abre nada: la frontera son los hijos de `repo:`. `depth=2` abre los directorios de primer nivel, `depth=3` también los de segundo, y `depth=all` abre todos (la frontera son solo archivos). Así "Nivel N" de la UI es `depth=N`, y "abrir todo" cabe en una URL corta aunque el repo tenga miles de carpetas.
  - `expanded` abre directorios por debajo del nivel; `collapsed` cierra directorios dentro del nivel (y con ellos todo lo que tienen debajo). Si un id está en los dos, gana `collapsed`.
  - `depth` es `all` (sin distinguir mayúsculas) o solo dígitos ASCII que valgan al menos 1: sin signo (`+5` da 400), sin espacios ni decimales. Un número mayor que el nivel más profundo abre todo. Cualquier otro valor da 400 `bad_request`.
- **Aristas**: cada arista de import archivo→archivo se eleva al ancestro de origen y al de destino que estén en la frontera. Las que quedan dentro del mismo nodo de la frontera se descartan, y las demás se suman por par.
- **Invariante, con test**: la suma de `weight` es igual al número de aristas archivo→archivo que cruzan entre nodos distintos de la frontera.
```json
{
  "generation": 3,
  "nodes": [ { "id": "dir:src/lib", "kind": "dir", "name": "lib", "path": "src/lib",
               "parent": "dir:src", "loc": 3000, "files": 40, "langs": {"typescript": 40},
               "fan_in": 0, "fan_out": 0, "unresolved": 0, "expandable": true } ],
  "edges": [ { "src": "dir:src/app", "dst": "dir:src/lib", "weight": 120,
               "confidence": { "exact": 30, "resolved": 90, "heuristic": 0 } } ],
  "externals": [ { "src": "dir:src/lib", "ext": "ext:npm:zod", "weight": 50 } ],
  "ignored": []
}
```
En los nodos del grafo, `fan_in` y `fan_out` son los grados ponderados dentro de este grafo agregado.

## GET /api/node?id=<id>
- **file**: el nodo, `imports` (`[{ target, specifier, line, confidence }]`), `importers` (`[{ source, line, confidence }]`), `externals` (`[{ ext, specifier, line }]`), `unresolved` (`[{ line, specifier, reason, detail }]`) y `package` (el id del paquete que lo contiene, o null).
- **dir**: el nodo, sus métricas, los 10 hijos con más LOC, las 10 dependencias salientes y las 10 entrantes más pesadas (agregadas a nivel de directorio hermano) y los paquetes externos más usados.
- **ext**: los archivos que lo importan.

## GET /api/packages
`[{ "id": "pkg:package.json", "name": "my-app", "kind": "npm", "dir": "dir:" , "manifest": "file:package.json", "notes": "..." }]`

## GET /api/issues
`{ "generation", "unresolved": [{ file, line, specifier, reason, detail }], "by_reason": {"not_found": 3}, "diagnostics": [...], "skipped": [{ path, reason, detail }] }`. Admite `?limit=` (por defecto 500), con `truncated` y `total`.

## GET /api/findings?limit=500
Hallazgos de arquitectura del snapshot actual, calculados con `cartograph-query` bajo el `cartograph.toml` del repo (formato en `docs/cartograph-toml.md`). Orden: primero las violaciones de reglas, después por severidad, tipo e id.
```json
{
  "generation": 3,
  "findings": [
    { "id": "layer_violation:ui->infra", "kind": "layer_violation", "severity": "error", "violation": true,
      "title": "layer ui depends on infra (forbidden): 1 import",
      "nodes": ["file:src/ui/app.ts"],
      "edges": [{ "src": "file:src/ui/app.ts", "dst": "file:src/infra/db.ts", "line": 2 }],
      "evidence": ["src/ui/app.ts:2 → src/infra/db.ts"],
      "metrics": { "imports": 1 } }
  ],
  "summary": { "total": 9, "violations": 5, "by_severity": { "error": 5, "warning": 2, "info": 2 },
               "by_kind": { "cycle": 1, "orphan": 3 } },
  "config": { "origin": "cartograph.toml", "error": null },
  "total": 9,
  "truncated": false
}
```
- `kind`: `layer_violation`, `fan_out_limit`, `cycle`, `folder_cycle`, `god_module`, `hub`, `sdp`, `orphan` o `deep_chain`. `severity`: `error`, `warning` o `info`. `violation: true` = rompe una regla de `cartograph.toml` (es lo que hace fallar `cartograph check`).
- `id` es estable entre indexados mientras el hallazgo exista: tipo más su ubicación principal (`cycle:file:src/a.ts`, `folder_cycle:dir:src/ui`, `layer_violation:ui->infra`, `sdp:dir:a->dir:b`).
- `title` y `evidence` están en inglés (los usa también la CLI y el MCP); la UI redacta su propio texto a partir de `kind`, `metrics` y `nodes`.
- `nodes`: ids de ubicación (`file:`, `dir:`, `repo:`), como mucho 60 (el resto en `nodes_more`). `edges`: aristas archivo → archivo que lo prueban, con la línea del import cuando la hay, `via_dir: true` si el import era de la carpeta (paquete de Go, comodín de Java…) y `type_only: true` si solo trae tipos (TypeScript `import type`, `export type … from`, `import("…")` en una posición de tipo; no `import { type A }` aunque todos los nombres sean `type`, porque con `verbatimModuleSyntax` queda `import {} from` y carga el módulo); como mucho 48, y el resto se cuenta en `edges_more` (ausente cuando es 0). Con `edges_more > 0`, `edges` es solo una muestra: la UI marca el hallazgo en sus nodos en vez de en las aristas. `evidence`: como mucho 12 líneas (el resto en `evidence_more`).
- Las dependencias solo de tipos cuentan para las reglas de capas, `max_fan_out`, huérfanos y cadenas profundas, pero no para ciclos (de archivos y de carpetas), PDE, cuellos de botella ni archivos dios: no se cargan al ejecutar (como `tsPreCompilationDeps: false` de dependency-cruiser). `cartograph query deps`, `impact` y `path` las siguen mostrando, marcadas `(type)`, porque cambiar un tipo sí rompe la compilación.
- `limit` por defecto 500, máximo 10000; `total` y `truncated` como en `/api/issues`.
- `config.origin` es `cartograph.toml` si el archivo existe; `config.error` explica por qué no se pudo usar (archivo y línea). Un `cartograph.toml` roto nunca tumba el servidor: los hallazgos se calculan con los valores por defecto.

## GET /api/search?q=<texto>&limit=50
Búsqueda difusa sobre rutas de archivos y directorios y sobre nombres de paquetes externos. Devuelve `[{ id, kind, path, name, score }]`, ordenado por score.

## GET /api/events (Server-Sent Events)
Eventos posibles:
- `hello`: `{generation, status}`, al conectar.
- `index-started`: `{reason: "startup" | "watch" | "manual", changed: [rutas]}`.
- `index-progress`: `{done, total}`, como mucho cada 250 ms.
- `index-finished`: `{generation, ms, changed: [rutas], counts}`.
- `index-error`: `{message}`.

Envía un keepalive (comentario `:`) cada 15 s. El frontend se reconecta solo (EventSource) y, al recibir `index-finished`, vuelve a pedir lo que esté mostrando.

## POST /api/reindex
Fuerza un reindexado completo. Responde `202 {"accepted": true}`.

## Frontend estático
- `GET /` y cualquier ruta que no empiece por `/api/` sirven el frontend embebido (`web/dist`) con fallback SPA a `index.html`.
- Si el binario se compiló sin `web/dist`, sirve una página HTML simple que dice cómo compilar el frontend (`npm --prefix web ci && npm --prefix web run build`, y luego `cargo build --release`). En ese caso la API sigue funcionando.

## Aclaraciones (backend)
Decisiones tomadas al implementar el backend donde el contrato era ambiguo o no calzaba con el store. Mandan sobre lo de arriba si chocan.

1. **Ids.** El store guarda `package:<manifiesto>` y `external:<eco:nombre>`; la API siempre emite `pkg:` y `ext:` (como dice el contrato) y traduce en la frontera. En la entrada (`?id=`, `?root=`, `expanded=`) acepta las dos formas.
2. **Raíz.** El directorio raíz es `repo:`. En la entrada, `dir:` (ruta vacía) es alias de `repo:`. Un paquete en la raíz trae `"dir": "repo:"` (no `"dir:"`, que no es un nodo).
3. **`generation` en todas las respuestas.** Por eso `/api/packages` y `/api/search` no son arrays sueltos: devuelven `{ "generation", "packages": [...] }` y `{ "generation", "results": [...] }`.
4. **Errores.** Cuerpo `{ "generation", "error": { "code", "message" } }`. En el 503 de `indexing` va además `"progress": { "done", "total" }` (o `null` si todavía se está recorriendo el árbol). Una ruta desconocida bajo `/api/` responde 404 `not_found` en JSON (nunca la SPA); un método no permitido, 405 con `bad_request`; parámetros mal formados, 400 `bad_request`; un pánico en un handler, 500 `internal`.
5. **Estado.** `status: "indexing"` solo hasta que termina el primer indexado de este proceso. Los reindexados posteriores no cortan el servicio: `status` sigue en `"ready"`, se sigue sirviendo el snapshot anterior y `meta` trae `"reindexing": true` y `progress`. Si el último indexado falló, `status: "error"` y `"error": "<mensaje>"`; si ya había un snapshot se sigue sirviendo, y si nunca hubo uno los endpoints de datos responden 500 `internal`. Antes del primer indexado, `indexed_at`, `index_ms` y `counts` son `null` y `languages` es `[]`. `indexed_at` es RFC 3339 en UTC; `repo.root` es la ruta absoluta con `/`.
6. **`counts`.** `imports` = todas las ocurrencias de import (`resolved` + `external` + `unresolved`); `resolved` = ocurrencias resueltas a un archivo o a un paquete de espacio de nombres de Python del repo; `external` = ocurrencias resueltas a un paquete externo.
7. **`/api/tree`.** `depth` cuenta desde el nodo raíz pedido (la raíz es profundidad 0). Un directorio en la profundidad límite que tiene hijos sale sin `children` y con `"truncated": true`. Los hijos van ordenados: primero directorios y después archivos, cada grupo por nombre. `root` que no existe da 404; `root` que es un archivo da 400. `fan_in`/`fan_out` de un archivo = archivos distintos que lo importan / que importa.
8. **`/api/graph`.**
   - `expanded` y `collapsed` aceptan ids repetidos y en cualquier orden, y también el parámetro repetido (`?expanded=a&expanded=b`). Un valor que es exactamente el id de un directorio se toma entero aunque tenga comas; si no, se parte por comas. `repo:` siempre está expandido. Va a `ignored` (como lista de ids, tal como llegaron y cada uno una sola vez aunque se repita; primero los de `expanded` y `collapsed` que no existen, después los de `expanded` que no quedaron abiertos) todo id de `expanded` que no sea un directorio existente, cuyo padre no esté abierto o que esté en `collapsed`, y todo id de `collapsed` que no sea un directorio existente o que sea `repo:`.
   - `weight` de una arista = número de aristas archivo→archivo distintas (pares de archivos) que la forman, no de ocurrencias. `confidence` reparte ese mismo número. En `externals`, `weight` = número de pares (archivo, paquete externo) distintos.
   - Imports de Python a un paquete de espacio de nombres (archivo→directorio): se elevan igual que un archivo, usando el nodo de la frontera que contiene a ese directorio (o el directorio mismo). Si el directorio está expandido, ningún nodo visible lo representa: no generan arista y se cuentan en `"hidden_namespace_imports": n`.
   - Invariante exacto (con test): suma de `weight` = aristas archivo→archivo que cruzan entre nodos distintos de la frontera + imports a espacio de nombres colocados que cruzan.
   - Cada nodo trae `parent` (`repo:` para el primer nivel). Un archivo trae `files: 1`, `langs: { <lang>: 1 }`, `lang` y `"expandable": false`.
   - Un archivo sin lenguaje detectado tiene `lang: null` y cuenta como `"other"` en `langs` y en `meta.languages`.
9. **`/api/node`.**
   - Todas las formas: `{ "generation", "node": {...}, ... }`. `node` tiene los mismos campos que en `/api/tree`, sin `children`.
   - file: `imports[].target` es un id (`file:`, `dir:` o `ext:`); `confidence` es la de la arista archivo→destino.
   - dir (y `repo:`): `children` (10 con más LOC, `{ id, kind, name, loc, files }`), `deps_out` y `deps_in` (`[{ id, weight }]`, 10 cada una) y `externals` (`[{ ext, weight }]`, 10). "Nivel de directorio hermano" = la frontera que resulta de expandir todos los ancestros del directorio; así el otro extremo es un hermano del directorio o de alguno de sus ancestros. `weight` cuenta pares de archivos, como en `/api/graph`.
   - ext: `importers: [{ source, specifier, line }]`.
   - pkg: `node` es el mismo objeto de `/api/packages`.
   - Id desconocido: 404.
10. **`/api/packages`.** `kind` es el tipo de manifiesto (`npm`, `pyproject`, `cargo`…); `notes` es el error de parseo del manifiesto o `null`.
11. **`/api/issues`.** `limit` (por defecto 500, máximo 100000) recorta `unresolved` y `skipped` por separado; `total` es `{ "unresolved": n, "skipped": n }` y `truncated` es `true` si se recortó alguna. `diagnostics` (`[{ path, detail }]`) no se recorta. Orden: por archivo y línea.
12. **`/api/search`.** `q` vacío devuelve `results: []`. `limit` por defecto 50, máximo 500. `kind` es `file`, `dir` o `ext`. `score` es un número mayor o igual que 0 (más alto, mejor); no es comparable entre consultas.
13. **SSE.** `changed` son rutas relativas al repo con `/`, ordenadas y como mucho 500; si hubo más, el evento trae `"changed_total": n`. `index-started.reason`: si en una misma corrida se juntan un reindexado manual y cambios del watcher, gana `"manual"`. `index-progress` también se emite durante el primer indexado.
14. **`POST /api/reindex`.** Siempre 202 `{ "accepted": true, "generation" }`. Si ya hay un indexado en curso, queda encolado exactamente uno más (lo mismo pasa con los cambios del watcher).
15. **Watcher.** Ignora el data dir (esté donde esté), `.git` y todo lo que ignora el recorrido (`.gitignore`, `.ignore`, `.git/info/exclude`, `node_modules`, `target`). Debounce de 300 ms. Si no puede arrancar, `meta.watching` es `false` y el servidor sigue.
16. **Frontend embebido.** `build.rs` embebe `web/dist` si existe al compilar (o la carpeta de `CARTOGRAPH_WEB_DIST`). Si no existe, compila igual y sirve la página de fallback. Cargo no se entera solo de que `web/dist` apareció después de una compilación sin él: tras el primer `npm run build`, hay que tocar `crates/cartograph-server/build.rs` o correr `cargo clean -p cartograph-server`. Cuando `web/dist` existe, cualquier cambio dentro de él recompila solo.
17. **Seguridad (DNS rebinding y CSRF).** Todo request, incluidos los archivos estáticos y `/api/events`, tiene que traer un `Host` que sea `127.0.0.1:<puerto>`, `localhost:<puerto>` o `[::1]:<puerto>`, con el puerto real del servidor. Si no lo trae, la respuesta es 403 con `code: "forbidden"`; ese código se suma a los de la regla de errores. Además, un request que no sea GET ni HEAD y traiga `Origin` tiene que venir del propio origen (`http://<uno de esos hosts>`). Uno sin `Origin` (curl, scripts) se acepta. El servidor no manda cabeceras CORS, así que otra página web no puede leer las respuestas.
18. **Archivos estáticos inexistentes.** Una ruta bajo `/assets/` o cuyo último segmento tiene extensión (`/favicon.ico`, `/img/logo.png`) y que no existe devuelve 404. Solo las rutas que parecen rutas del cliente (`/map/dir/src`) caen en `index.html`, o en la página de fallback si el binario se compiló sin frontend.
19. **Hallazgos.** `/api/meta` agrega a `counts` dos campos: `findings` (hallazgos que son problemas: severidad `error` o `warning`) y `findings_info` (los informativos: cuellos de botella, dependencias estables, cadenas profundas, código que solo usan las pruebas). La UI suma `findings` al contador "N problemas" junto con imports sin resolver, diagnósticos y omitidos; pone el punto magenta en la tarjeta o caja que contiene cada ubicación de un hallazgo que es problema, pinta de magenta las aristas dibujadas que lo prueban (solo el color: el layout no cambia) y muestra el texto de cada hallazgo en el tooltip, en el panel de detalle y en la pestaña "Hallazgos" del cajón de problemas. `/api/findings` también responde `503 indexing` antes del primer indexado.
20. **Flujos: reglas comunes.** Los endpoints de flujos leen la capa de símbolos del snapshot, que el servidor carga del store junto al grafo y la historia git en cada indexado (también en la publicación que sigue a la actualización de git). Un índice sin capa de símbolos (escrito antes de que existiera) responde **200** con `{ "generation", "indexed": false }` (y en `/api/entries` además `entries: []`, `total: 0`, `hidden: 0`, `truncated: false`): la UI pide reindexar sin que el navegador registre un error. Los ids de símbolo son los del store: `sym:<ruta>#<nombre calificado>` (`#` va como `%23` en la URL). Un símbolo o archivo inexistente da 404 `not_found`; un nombre que comparten varios símbolos, 400 `bad_request` con los candidatos. Cada respuesta lleva `generation`; tras `index-finished` la UI repite el mismo pedido (con los mismos `expand`) y conserva la cámara.
21. **`GET /api/entries`.** Dónde empiezan los flujos, sin los que están en árboles de pruebas, fixtures y ejemplos (`test`, `tests`, `fixtures`, `examples`, `testdata`…; se cuentan en `hidden`). Orden: comandos de CLI, rutas HTTP, `main`, escena principal y autoloads de Godot; dentro de cada tipo por archivo y línea. Como mucho 1000 (`truncated`).
    ```json
    { "generation": 3, "indexed": true, "total": 28, "hidden": 41, "truncated": false,
      "entries": [ { "kind": "cli", "name": "index", "file": "crates/cli/src/main.rs", "line": 38,
                     "target": "sym:crates/cli/src/main.rs#Command", "root": "sym:crates/cli/src/main.rs#cmd_index" } ] }
    ```
    `kind`: `cli`, `http`, `main`, `godot_main_scene` o `godot_autoload`. `target` es lo que guardó el indexador; `root` es donde empieza el flujo: la función de `target`; si `target` es un tipo (el `enum` de `clap`, una clase controladora), la función manejadora del mismo nombre en ese archivo (`cmd_index`, `handleIndex`, `run_index`, `index`…), o en el repo si hay una sola; si no hay, el `main` del archivo; si tampoco, `file:<ruta>` (las llamadas de nivel de módulo del archivo). En una ruta HTTP el nombre sale de la última parte literal de la ruta, nunca del método: `GET /`, `GET /:id` o `*` no nombran ninguna función. El `root` de cada entrada se calcula una vez por indexado.
22. **`GET /api/flow?from=<id>&depth=4&max=60&expand=<id>…`.** El flujo de llamadas desde `from` (un `sym:`, `ruta#nombre`, un nombre único o `file:<ruta>`), en anchura: cada función muestra hasta 8 de sus llamadas en el orden en que las hace por primera vez, hasta `depth` llamadas (1–8, por defecto 4) y `max` funciones (10–300, por defecto 60). Lo que queda fuera se cuenta por función en `more` ("+N más"). `expand` (repetible o separado por comas, en el orden en que se abrieron) abre del todo una función: hasta 40 llamadas, aunque pase la profundidad o el presupuesto (tope 300). Las expansiones se suman al flujo base (el que se ve sin ellas), que se calcula primero: abrir una función nunca quita las que ya se mostraban. Sus llamadas entran como tarjetas con su propio `more`, y una función abierta lleva `"open": true`; si aun así le queda `more`, es el tope del flujo (volver a abrirla no muestra nada nuevo). Una función llamada desde varios lugares es un solo nodo con varias aristas (un DAG; un ciclo si el código es recursivo entre funciones; la recursión directa no se dibuja).
    ```json
    { "generation": 3, "indexed": true, "root": "sym:crates/cli/src/main.rs#cmd_index", "depth": 4, "max_nodes": 60, "truncated": true,
      "nodes": [ { "id": "sym:crates/index/src/lib.rs#index_to_store", "name": "index_to_store", "kind": "function",
                   "file": "crates/index/src/lib.rs", "line": 499, "end_line": 560, "callers": 33, "depth": 1,
                   "callees": 11, "more": 3, "finding": "warning", "lane": "crates/index" } ],
      "edges": [ { "src": "sym:…#cmd_index", "dst": "sym:…#index_to_store", "confidence": "resolved", "line": 268, "calls": 1 } ],
      "lanes": [ { "id": "crates/cli", "label": "crates/cli" } ] }
    ```
    - `name`: `Contenedor.nombre` para un método. `callers`: sitios de llamada que llegan a la función en todo el repo. `callees`: funciones distintas que llama. `finding`: el peor hallazgo que es problema (`error` o `warning`) sobre su archivo; ausente si no hay.
    - `lane` (carril): la carpeta del paquete más profundo que declara un manifiesto (sin contar la raíz); si no hay, la carpeta de primer nivel, o las dos primeras bajo una raíz de código (`src/ui`, `lib/core`); `""` para archivos en la raíz. Si todo el flujo cae en un solo carril, se parte por el siguiente nivel de carpetas (`src/server` → `src/server/api`, `src/server/auth`; los archivos de la propia carpeta se quedan en ella). `lanes` en orden de aparición (el de `from` primero).
    - Presupuesto: nivel por nivel, y dentro de un nivel las funciones que más llaman se siguen primero, así los nodos se gastan en las que hacen el trabajo antes que en ayudantes chicos (`new`, `default`). La UI pide `max=30`; el resto se abre con clic ("+N").
    - Aristas y `more` se calculan sobre el conjunto final: toda llamada entre dos tarjetas del flujo es una arista, y `more` cuenta las funciones distintas que llama fuera del flujo.
    - Aristas: `confidence` es la más alta de los sitios que se juntan (`exact`, `resolved`, `heuristic`); `line`, la primera llamada en el archivo de `src`; `calls`, cuántos sitios.
23. **`GET /api/symbol?id=<id>`.** Una función, o el código de nivel de módulo de un archivo con `id=file:<ruta>` (un script, una escena de Godot, una entrada sin manejadora: `kind: "file"`, `name` el nombre del archivo, `line` 1 y `end_line` sus líneas no vacías, `exported: false`): `symbol` (`id`, `name`, `kind`, `file`, `line`, `end_line`, `exported`, `lane`); `callers` con `production` (sitios fuera de pruebas, más confiables primero y luego por archivo y línea, como mucho 30; cada uno `{ file, line, caller, caller_name, confidence }`), `production_total`, `production_files` y, de las pruebas, solo `test_sites` y `test_files`; `callees` (como mucho 40: `{ id, name, file, line, confidence, calls, lane }`), `callees_total` y `callee_lanes`; `git` (`{ commits, recent, window_days, last_time, authors: [{ name, commits }] (3), authors_total }` del archivo, o `null` sin capa git); `finding` y `findings` (títulos, como mucho 3) del archivo; `entries`: las entradas cuyo flujo empieza aquí.
24. **`GET /api/file-flows?id=file:<ruta>`.** Lo que lista "Ver flujos" en el Plano: `functions` (funciones y métodos del archivo, más llamados primero, como mucho 40; `{ id, name, kind, line, callers, callees }`), `functions_total` y `entries`: las declaradas en el archivo y las que llegan a alguna de sus funciones (se recorren los llamadores hacia atrás, fuera de pruebas, hasta 8 llamadas y 4000 funciones; `partial: true` si se cortó). Como mucho 40, primero las del propio archivo.
25. **Funciones en `/api/search`.** `kinds=<lista>` (`dir`, `file`, `ext`, `sym`, separados por comas) elige qué buscar; sin `kinds` la respuesta es la de siempre (carpetas, archivos y librerías). `sym` busca funciones y métodos por nombre (`nombre`, `Clase.método`, `ruta#nombre`): primero los nombres exactos, después los que empiezan así y luego los que lo contienen, los de pruebas al final y, dentro de cada grupo, los más llamados. Cada resultado `sym` trae `{ id, kind: "sym", path: <archivo>, name, line, symbol_kind, callers }`. Un tipo desconocido da 400.
