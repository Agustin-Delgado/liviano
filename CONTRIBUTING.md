# Contribuir a Nea

Gracias por querer sumar. Nea es chico a propósito: antes de una función
nueva grande, abrí un issue con el pedido para charlarlo, así nadie trabaja en
algo que después no entra.

## Compilar

Hace falta Windows 10 u 11, Rust estable con el destino
`x86_64-pc-windows-msvc`, las herramientas de compilación de Visual Studio
(MSVC) con el Windows SDK (por `rc.exe`) y el runtime de WebView2.

```powershell
cargo build            # desarrollo: se llama "Nea (desarrollo)"
cargo build --release  # la versión que se publica
```

Antes de abrir un PR, lo mismo que corre la CI:

```powershell
cargo fmt --check
cargo clippy -- -D warnings
cargo build --release
```

Más detalle (control por script, diagnóstico, empaquetado) en
[docs/compilar.md](docs/compilar.md).

## Estilo

- **El código va en inglés:** nombres, comentarios y mensajes internos.
- **Los textos de la interfaz van en español:** menús, Ajustes, avisos. La
  documentación y los mensajes de commit, también.
- Un comentario explica por qué algo es así, no repite lo que dice la línea.
- `cargo fmt` con el `rustfmt.toml` del repositorio, y sin warnings de clippy.
  Si un `allow` hace falta, va con su `reason`.
- La memoria es la prioridad: algo que cuesta memoria en reposo tiene que
  justificarse con una medición ([docs/medicion.md](docs/medicion.md)).
- Nada sale a la red sin que la persona lo pida, y no hay telemetría.

## Tests

Los tests no están en este repositorio: viven en un repositorio privado del
mantenedor, junto con el código. Por eso un PR se valida del lado del
mantenedor antes de entrar; la CI de acá sólo comprueba que compile, el
formato y clippy. Si tu cambio necesita una prueba, describí en el PR cómo lo
probaste a mano.

## Pull requests

- Un PR, un cambio. Mejor varios chicos que uno grande.
- Contá qué cambia para quien usa Nea y cómo lo probaste.
- Si toca la interfaz, sumá una captura en claro y en oscuro.
- Los fallos de seguridad no van en un issue ni en un PR: ver
  [SECURITY.md](SECURITY.md).

Al contribuir aceptás que tu aporte se publique bajo la [licencia MIT](LICENSE)
y que sigas el [código de conducta](CODE_OF_CONDUCT.md).
