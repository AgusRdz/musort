# musort

Organiza una biblioteca de música plana (`Artista/Canción.mp3` + `.lrc`) para que se vea bien en Jellyfin y en un reproductor portátil (probado con un FiiO JM21), y mantiene la tarjeta del reproductor sincronizada con la biblioteca.

Es un solo binario. Nada se escribe sin `--apply`, y lo que se quita o cambia se puede revertir con `musort undo`.

## Compilar

Necesitas Rust (stable):

```sh
brew install rustup && rustup-init -y   # si no lo tienes
cargo build --release
```

El binario queda en `target/release/musort`.

## Instalar en el PATH

```sh
mkdir -p ~/.local/bin
install -m 755 target/release/musort ~/.local/bin/musort
```

Si `~/.local/bin` no está en tu PATH, agrégalo a tu shell (`~/.zshrc` o `~/.bashrc`) y recarga:

```sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
```

Para actualizar, vuelve a compilar y repite el `install`.

## Uso

```sh
musort doctor /ruta/a/Music            # reporte de problemas (solo lectura)
musort clean  /ruta/a/Music            # carpetas vacías, basura de macOS/Android, letras sin canción
musort fix    /ruta/a/Music            # tags y nombres de archivo/carpeta
musort review /ruta/a/Music            # pantalla para lo que fix no decide solo
musort sync   /ruta/a/Music /Volumes/TARJETA
musort undo   /ruta/a/Music            # revierte la última operación
```

`clean`, `fix` y `sync` muestran lo que harían; agrega `--apply` para hacerlo. `doctor`, `fix` y `sync` aceptan `--all` para listar todo en vez de los primeros elementos.

### doctor

Lista lo que está mal: canciones sin `album_artist`, varios artistas metidos en un solo valor (`"A, B"`), carpetas que no coinciden con el artista del tag, posibles alias de un mismo artista, duplicados, URLs en los tags, canciones sin género, año, carátula o letra, y basura del sistema.

### clean

Mueve a la papelera carpetas sin música, carpetas y archivos que crean macOS y Android (`.DS_Store`, `._*`, `.Spotlight-V100`, `Android/`…) y letras `.lrc` cuya canción ya no existe. Una carpeta con archivos que no son música no se toca.

### fix

- Separa colaboraciones en varios artistas (ID3v2.4, un valor por artista) y llena `album_artist` con el artista principal. Así Jellyfin liga la canción a cada artista y el FiiO la agrupa bajo el principal sin crear artistas falsos como `"A, B"`.
- Unifica la forma de escribir al artista dentro de cada carpeta.
- Quita publicidad de los tags (`- www.sitio.com`, `Título | Banda 2016`, `(feat. X)` repetido).
- Renombra archivos según el título y carpetas según el artista, con nombres válidos en FAT/exFAT.
- Las canciones cuyo tag dice un artista distinto al de su carpeta no se tocan; se resuelven con `review`.
- `--unicode` también convierte a NFC los nombres guardados en NFD. Es cosmético y Jellyfin verá esas rutas como canciones nuevas (pierde historial y favoritos), por eso no se hace por defecto.

### review

Pantalla en la terminal con dos pestañas (Tab para cambiar):

- **Artista equivocado**: `c` usa el artista de la carpeta, `t` usa el del tag y mueve la canción a esa carpeta, `e` para escribir el nombre.
- **Posibles alias**: `1` une todo en la carpeta de la izquierda, `2` en la de la derecha, `e` para escribir el nombre final, `d` si son artistas distintos (se recuerda en `.musort.json` y no se vuelve a preguntar).

`x` quita una decisión, `a` dos veces aplica, `q` sale. Después corre `musort fix` para limpiar títulos.

### sync

Copia a la tarjeta solo la música y letras nuevas o cambiadas, y borra de la tarjeta lo que ya no está en la biblioteca. Guarda un índice `.musort-sync.json` en la tarjeta para no volver a compararla completa.

- Solo borra música, letras y archivos ocultos; cualquier otra cosa se conserva y se reporta. Nunca entra a `Android/`, `DCIM/`, `Music/` ni a las carpetas ocultas de la raíz.
- Si la tarjeta tiene una letra que la biblioteca no (el reproductor puede descargarlas), la copia a la biblioteca en vez de borrarla.
- Borra los `._*` que macOS crea al escribir en exFAT.
- Usa un lector de tarjetas. Montar el reproductor por MTP (MacDroid y similares) es lento y falla al leer muchas carpetas.

### undo

Cada `clean`, `fix --apply` o cambio desde `review` guarda un registro en `.musort-trash/<fecha>/` dentro de la biblioteca: lo que se quitó se mueve ahí en vez de borrarse, y los renombres y cambios de tags quedan anotados. `undo` revierte la operación más reciente; repítelo para ir más atrás. Cuando estés seguro, borra `.musort-trash` a mano. Jellyfin ignora las carpetas que empiezan con punto.

## Notas

- El primer escaneo de una biblioteca en red lee los tags de todos los archivos y puede tardar un par de minutos; después se usa un índice en `~/Library/Caches/musort/` (o `~/.cache/musort/`) y solo se releen los archivos que cambiaron.
- Escribir tags puede reescribir el archivo completo la primera vez (si el tag no tiene espacio libre). Por red eso es lento; las siguientes escrituras son rápidas.
- Formatos: lee mp3, flac, m4a, ogg, opus y wav; escribe tags en mp3 y flac.
- macOS y Linux.
