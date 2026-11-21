# Extensiones

Nea carga extensiones de Chrome a través de WebView2. Para eso usa
`AreBrowserExtensionsEnabled` en el entorno y `AddBrowserExtension` /
`GetBrowserExtensions` en el perfil. Este documento recoge lo que se probó en
el runtime **157.0.4341.60**, lo que funciona y lo que no. Es la lista de
limitaciones conocidas.

## Costo en memoria

El soporte de extensiones se decide **al crear el motor**. Por eso Nea lo
prende sólo si hay al menos una extensión instalada y habilitada. Sin
extensiones el motor arranca igual que antes, sin ese flag y sin procesos de
más.

Cambiar eso en caliente no es posible. Si se instala la primera extensión, o
se deshabilita la última, el cambio se aplica en el próximo arranque, y el
menú y Ajustes lo avisan.

Medido con la misma pestaña (example.org), memoria privada de todo el árbol de
procesos:

| motor | procesos | privado total | sin GPU |
|---|---|---|---|
| sin extensiones | 7 | 166,9 MB | 90,5 MB |
| con la extensión de prueba habilitada | 8 | 177,1 MB | 100,4 MB |

- Cada extensión con service worker suma un proceso renderer propio, de unos
  **20 MB** para una extensión mínima.
- Una extensión pesada, como un bloqueador con listas, cuesta más.
- La GPU varía ±10 MB entre corridas y no depende de las extensiones.

Por eso el menú termina con "Cada extensión habilitada suma procesos y
memoria", y Ajustes repite la advertencia.

## APIs probadas

La extensión de prueba (que no forma parte de este repositorio) es MV3, con service
worker, content script, popup y opciones. Llama a cada API desde el service
worker y deja el informe en la página.

| API | Resultado |
|---|---|
| `storage.local`, `storage.sync` | Funciona |
| `runtime` (mensajes content ↔ SW, popup ↔ SW) | Funciona |
| `tabs.query` | Funciona |
| `tabs.create` | Abre la pestaña: llega a Nea como `NewWindowRequested` y Nea la abre como pestaña propia. La promesa devuelve `undefined`. |
| `windows.getAll` | Funciona |
| `bookmarks`, `history`, `downloads` | Responden, pero con los datos **del motor**, no con los de Nea. Nea guarda sus propios marcadores, historial y descargas, así que la extensión ve listas vacías o distintas. |
| `sidePanel.getOptions` | Responde, pero no hay panel lateral donde dibujarlo |
| `sidePanel.open` | Falla: pide gesto del usuario y no hay panel |
| `identity.getRedirectURL` | Funciona |
| `identity.getAuthToken` | No soportada ("not supported on Microsoft Edge") |
| `identity.launchWebAuthFlow` | La llamada no interactiva falla ("Authorization page could not be loaded"). La interactiva no se verificó. |
| `nativeMessaging` | Funciona, con el host registrado en `HKCU\Software\Microsoft\Edge\NativeMessagingHosts\<nombre>` o en `HKCU\Software\Google\Chrome\NativeMessagingHosts\<nombre>` |
| `action.setBadgeText`, `action.openPopup` | Existen, pero no hay barra de herramientas: el badge no se ve y el popup no se abre |
| `contextMenus`, `alarms`, `scripting.executeScript`, `notifications`, `cookies`, `webRequest`, `declarativeNetRequest`, `i18n`, `permissions`, `offscreen` | Funcionan |
| Navegar a `chrome-extension://<id>/popup.html` u `options.html` desde Nea | Funciona |

## Limitaciones y cómo se resolvieron

- **Popup, opciones y panel lateral se abren en una pestaña.**
  - WebView2 no ofrece barra de herramientas ni la burbuja del popup.
  - En el menú de la pieza de rompecabezas, cada extensión ofrece "Abrir
    ventana emergente", "Opciones" y "Panel lateral", y cada una se abre como
    una pestaña normal.
  - Las extensiones fijadas se muestran con su inicial al lado del botón.
    Hacer clic en la inicial abre el popup.
  - Consecuencia: el popup **se ve a sí mismo como la pestaña activa**. Si un
    popup actúa sobre "la pestaña actual", como el de uBlock Origin Lite, dice
    que no es un sitio web.
- **Sin badge ni contador.** Lo que la extensión pinta con `action` no se ve.
- **Datos del navegador.** Las extensiones de marcadores, historial o
  descargas operan sobre el almacén del motor, no sobre el de Nea.
- **Recargar.**
  - Se vuelve a llamar `AddBrowserExtension` con la misma carpeta. El motor
    la recarga en el lugar, con el mismo id, conserva su `storage` y toma la
    versión nueva (probado de 1.0 a 1.1).
  - Quitar y volver a agregar también recarga, pero borra el `storage`. Por
    eso no se usa.
  - Durante la recarga, Chromium manda las pestañas de la extensión a
    `chrome-search://local-ntp/local-ntp.html`, que en WebView2 es
    `ERR_INVALID_URL`. Por eso Nea guarda esas pestañas antes de recargar
    y las devuelve a su dirección después.
- **Deshabilitar** cierra las pestañas `chrome-extension://` de esa extensión
  y detiene su service worker.
- **Ids.**
  - Una extensión sin empaquetar recibe un id derivado de la carpeta.
  - En las de la Web Store, Nea escribe la clave pública del paquete
    (`key`) en el `manifest.json` desempaquetado. Así el motor usa el id de
    la tienda, y las páginas y el `storage` coinciden con los de Chrome.
- **Extensiones internas del motor.** El Microsoft Clipboard Extension y el
  visor de PDF vienen con WebView2 y Nea nunca las toca. Sólo se quitan
  del motor los ids que Nea mismo registró y la persona quitó.

## Instalar desde la Chrome Web Store

1. Pegar el enlace en el campo único. Sirven los dos formatos:
   - `chromewebstore.google.com/detail/<slug>/<id>`
   - `chrome.google.com/webstore/detail/<slug>/<id>`

   Nea ofrece la fila "Instalar extensión de la Chrome Web Store". En
   una página de la tienda, el menú de extensiones ofrece además "Instalar la
   extensión de esta página".
2. Nea descarga el CRX3 de `clients2.google.com/service/update2/crx`, con
   `prodversion` igual a la versión del runtime instalado.
3. Verifica el paquete antes de escribir nada:
   - Formato `Cr24` versión 3.
   - Todas las firmas (RSA PKCS#1 y ECDSA P-256 con SHA-256, mediante CNG).
   - Al menos una firma del desarrollador cuya clave da el `crx_id` firmado.
   - La firma de la Web Store, cuyo hash de clave pública es el `kPublisherKeyHash`
     de Chromium.
   - Que el id coincida con el del enlace.
4. Descomprime en `%APPDATA%\Nea\extensions\<id>`. El zip se lee a mano:
   rechaza rutas con `..` o absolutas y entradas cifradas o zip64. Después
   llama a `AddBrowserExtension`.
5. Si el motor arrancó sin soporte de extensiones, queda registrada y se carga
   en el próximo arranque, con aviso.

Las extensiones instaladas y sus estados (habilitada, fijada) están en
`extensions.json`, dentro de la carpeta de datos.

## Para desarrolladores

- **Cargar sin empaquetar.** Está en el menú y en Ajustes, como "Cargar
  extensión sin empaquetar…". Abre un selector de carpetas (`IFileOpenDialog`
  con `FOS_PICKFOLDERS`). La carpeta no se copia: se carga desde donde está.
- **Recargar desde la carpeta.** También en el menú y en Ajustes. Lee de nuevo
  el `manifest.json` y recarga la extensión conservando su `storage`.
