# cartograph.toml: reglas de arquitectura

`cartograph.toml` es opcional y vive en la raíz del repositorio. Declara la arquitectura que el proyecto quiere tener (capas, dependencias permitidas o prohibidas, límites) y ajusta los umbrales de los hallazgos. Sin el archivo, cartograph igual encuentra ciclos, módulos dios, cuellos de botella, código muerto y cadenas profundas; lo que no puede saber sin él es qué dependencias están prohibidas.

Lo leen `cartograph check`, `cartograph query findings` y `cartograph serve` (en cada indexado, así que editarlo con `serve` abierto recalcula los hallazgos). `--config <archivo>` usa otro archivo en lugar del de la raíz. `cartograph infer` propone uno a partir del grafo.

## Ejemplo

```toml
# Archivos fuera de todo hallazgo.
ignore = ["**/generated/**", "src/legacy/**"]

[[layers]]
name = "ui"
paths = ["src/ui/**"]
forbid = ["infra"]              # ui nunca depende de infra

[[layers]]
name = "core"
paths = ["src/core"]            # una ruta sin comodines: esa carpeta y todo lo que tiene debajo
allow = []                      # core no depende de ninguna otra capa

[[layers]]
name = "infra"
paths = ["src/infra/**", "src/db/*.ts"]
allow = ["core"]                # infra solo puede depender de core

[rules]
max_fan_out = 25                # ningún archivo depende de más de 25 archivos
no_cycles = ["src/core/**"]     # o `true` para todo el repo

[thresholds]
fence = 1.5
god_min_loc = 400

[findings]
disable = ["sdp"]
```

## Rutas
Todas las rutas son relativas a la raíz del repo y usan `/` (también se acepta `\` y un `./` inicial).
- Con comodines (`*`, `?`, `[…]`, `{a,b}`) es un glob: `*` no cruza carpetas y `**` sí. `src/ui/**` es todo lo que hay bajo `src/ui`; `src/db/*.ts` son solo los `.ts` directos de `src/db`.
- Sin comodines es una carpeta o un archivo: `src/core` (o `src/core/`) es la carpeta y todo lo que tiene debajo.

## `[[layers]]`
Cada capa tiene:
- `name` (obligatorio, único).
- `paths` (obligatorio, al menos una ruta). Un archivo pertenece a la **primera** capa, en el orden del archivo, cuyas rutas lo cubren. Los archivos que no caen en ninguna capa no se revisan.
- `allow` (opcional): las únicas capas de las que puede depender, además de sí misma. Sin `allow`, puede depender de cualquiera que no esté en `forbid`. `allow = []` significa que no depende de ninguna otra capa.
- `forbid` (opcional): capas de las que nunca puede depender.

Las dependencias dentro de una misma capa siempre están permitidas. Una capa no puede prohibirse a sí misma ni tener una capa en `allow` y en `forbid` a la vez. Cada par de capas que se viola es un hallazgo `layer_violation` con todos los imports que lo cruzan (`archivo:línea → archivo`).

## `[rules]`
- `max_fan_out` (entero ≥ 1): un archivo que depende de más archivos distintos es un hallazgo `fan_out_limit`.
- `no_cycles`: `true` (todo ciclo es una violación), `false` o `[]` (ninguno), o una lista de rutas: un ciclo es una violación cuando algún archivo de su ciclo más corto cae en esas rutas. Un ciclo entre archivos Java de un mismo paquete que solo se nombran sin import queda dentro del paquete, la unidad de diseño (como en ArchUnit): es informativo y nunca una violación, tampoco en `check --base`; basta un import entre paquetes para que vuelva a contar. Esas dependencias sin import sí cuentan para las capas y `max_fan_out`: son dependencias reales entre archivos.

## `[thresholds]`
Los hallazgos que miden el repo contra sí mismo usan la valla de Tukey sobre la distribución del propio repo (Q3 + `fence` × rango intercuartílico) más un piso pequeño. Todos son opcionales:

| Clave | Por defecto | Qué controla |
|---|---|---|
| `fence` | 1.5 | Multiplicador de la valla de Tukey (módulos dios, cuellos de botella, cadenas). |
| `god_min_fan_in` | 8 | Módulo dios: mínimo de archivos que lo usan. |
| `god_min_fan_out` | 8 | Módulo dios: mínimo de archivos que usa. |
| `god_min_loc` | 300 | Módulo dios: mínimo de líneas. |
| `max_hubs` | 5 | Máximo de cuellos de botella reportados. |
| `hub_min_share` | 0.05 | Cuello de botella: parte mínima (0 a 1) de los caminos de dependencias que pasan por el archivo. |
| `chain_min` | 10 | Cadena profunda: mínimo de archivos. |
| `max_chains` | 3 | Máximo de cadenas profundas reportadas. |
| `sdp_margin` | 0.4 | Dependencias estables: diferencia mínima de inestabilidad (0 a 1). |
| `sdp_min_files` | 3 | Dependencias estables: archivos de código mínimos de una carpeta para contar. |
| `sdp_min_dependents` | 3 | Dependencias estables: la carpeta estable necesita al menos estos dependientes. |
| `max_sdp` | 10 | Máximo de hallazgos de dependencias estables. |
| `coupling_min_shared` | 5 | Acoplamiento oculto: mínimo de commits compartidos (además de la valla del repo sobre todos los pares). |
| `coupling_min_degree` | 0.5 | Acoplamiento oculto: grado mínimo (0 a 1) = commits compartidos ÷ promedio de los commits de ambos archivos (la medida de code-maat). |
| `max_coupling` | 10 | Máximo de hallazgos de acoplamiento oculto (los más fuertes primero). |

## `[findings]`
- `disable`: tipos de hallazgo que no se calculan. Tipos: `layer_violation`, `fan_out_limit`, `cycle`, `folder_cycle`, `god_module`, `hub`, `sdp`, `orphan`, `deep_chain`, `hidden_coupling`.

## `ignore`
Lista de rutas cuyos archivos quedan fuera de todos los hallazgos (no aparecen ni rompen reglas). No cambia el índice ni las consultas como `explain` o `deps`.

## En un workspace

En un `cartograph-workspace.toml` (ver [workspace.md](workspace.md)) valen `[[layers]]`,
`[rules]`, `ignore`, `[thresholds]` y `[findings]` con esta misma gramática; las rutas
empiezan por el nombre de un miembro (`paths = ["api"]`) y juzgan todas las dependencias.
El `cartograph.toml` propio de cada miembro se rebasa: sus capas se llaman
`<miembro>.<capa>` (`api.web`), sus rutas quedan bajo la carpeta del miembro y juzgan solo
las dependencias dentro de él (`layer_violation:api.web->api.db`). Su `ignore`,
`no_cycles` y `max_fan_out` se aplican rebasados; sus `[thresholds]` y `[findings]` no se
leen. `[workspace]`, `[[member]]` y `[bridges]` solo valen en el archivo del workspace.

## Errores
Cualquier clave desconocida, tipo incorrecto, capa inexistente o glob inválido es un error con archivo, línea y clave, por ejemplo:

```
cartograph.toml:7: layers[1].allow: unknown layer "cor" (layers: ui, core, infra)
cartograph.toml:2: unknown field `max_fanout`, expected `max_fan_out` or `no_cycles`
```

`cartograph check` sale con código 2 si el archivo no es válido (y con 1 si hay violaciones, 0 si no). `cartograph serve` no se cae: usa los valores por defecto y muestra el error en `GET /api/findings` (`config.error`) y en el cajón de problemas.

## Qué miran los hallazgos
- Solo dependencias entre archivos del repo: `import`, `extends`, `implements`, `uses` y las conexiones de señales de Godot. Un import de una carpeta (paquete de Go, comodín de Java, `using` de C#, paquete de espacio de nombres de Python) alcanza a los archivos directos de esa carpeta del mismo lenguaje, sin sus pruebas. Sin import también hay dependencia (`uses`): de un archivo Java al que declara un tipo de su mismo paquete que nombra (también desde `src/test/`), y de una prueba de Go (`_test.go`) a su paquete.
- Las pruebas no cuentan: dependen del código y nunca al revés, y solo agregarían ruido (ciclos a través de ayudantes de prueba, fan-in de las suites). Módulos dios, cuellos de botella, dependencias estables y cadenas miran solo archivos de código (no scripts, configuración ni pruebas).
- Que un módulo raíz declare o reexporte sus submódulos (`lib.rs`/`mod.rs`/`foo.rs` → `foo/…` en Rust, `__init__.py` en Python) es estructura, no dependencia; los barriles de TS/JS (`index.ts`) sí cuentan, porque un ciclo a través de ellos rompe en ejecución.
- Huérfanos: por archivo en TS/JS, Python, Rust, GDScript y cabeceras de C/C++; por paquete en Go; no se reportan en Java, Kotlin ni C# (las clases de un paquete se ven sin import y los frameworks conectan por escaneo; hace falta la capa de símbolos). Nunca son huérfanos los puntos de entrada: `main`, `index`, `app`, `server`, `cli`, `preload`, `worker`, `__main__`…, archivos sueltos en la raíz de un paquete, lo que `package.json` ejecuta o expone (`main`, `bin`, `exports`, `scripts`), rutas de frameworks (`pages/`, `app/**/page.tsx`, `routes/`), raíces de crate y binarios de Cargo, unidades de traducción de C/C++, autoloads y escena principal de Godot, `*.d.ts`, historias, pruebas, scripts, configuración y archivos bajo `docs/`, `examples/`, `public/`, `static/`, `resources/` o `vendor/`.

## Acoplamiento oculto (`hidden_coupling`) y la historia de git
`cartograph index` (y `serve`, después de dibujar el mapa) lee la historia de git del repo con gix, sin el `git` de la línea de comandos; `--no-git` la omite. Solo lee los commits nuevos desde la última pasada; si la historia se reescribió (rebase, reset, otra rama) la vuelve a leer entera. Un clon superficial (`--depth`) se marca como historia parcial. Sin `.git` no hay historia y nada falla.

- **Churn**: commits que no son merge y tocan el archivo (lo que cuenta `git log --no-merges --follow`), siguiendo renombres con 50 % de similitud como git, más líneas agregadas y quitadas (las de `git log --numstat`). "Recientes" son los de los 90 días que terminan en el commit más nuevo del repo (no en la fecha de hoy, así los números no cambian solos).
- **Autores**: nombre (después de `.mailmap`), nunca el correo; un hash del correo une a una persona que escribe dos nombres, y un mismo nombre con dos correos también es una persona. Por eso puede diferir de `git shortlog -sn`, que agrupa solo por nombre. El *bus factor* es la menor cantidad de autores que suman el 50 % (y el 80 %) de los commits del archivo.
- **Co-cambio**: dos archivos cambiados por un mismo commit. Como en code-maat, no cuentan los merges ni los commits de más de 50 archivos (formateos, importaciones masivas). La confianza de B para A es la parte de los commits de A que también cambiaron B.
- **Hotspots** (`cartograph query hotspots [--within carpeta]`): churn × complejidad por indentación de la versión en HEAD (suma de niveles de sangría de las líneas no vacías), solo archivos de código.

Un **acoplamiento oculto** es un par de archivos de código que cambian juntos a menudo pero sin ningún camino de dependencias entre ellos en ningún sentido (ni directo ni a través de otros archivos, contando pruebas e imports de solo tipos): comparten un contrato que el grafo no muestra (un formato de mensaje, un nombre de canal, lógica duplicada). Un par entra cuando sus commits compartidos superan la valla de Tukey sobre todos los pares del repo y `coupling_min_shared`, y su grado llega a `coupling_min_degree`. Solo participan archivos de lenguajes con extracción de imports (TS/JS, Python, Rust, Go, Java, C#, C/C++, GDScript y escenas de Godot), nunca pruebas ni archivos de `ignore`. En Go, Java y C# los archivos de una misma carpeta se ven sin import (un paquete o espacio de nombres), así que esos pares no cuentan. Es informativo (`info`), nunca una violación.
