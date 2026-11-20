# Seguridad

## Cómo reportar una vulnerabilidad

**No abras un issue público.** Un fallo de seguridad se reporta en privado, así
quien usa Liviano tiene la corrección antes de que el problema se conozca.

- **Preferido:** en el repositorio de GitHub, pestaña *Security* → *Report a
  vulnerability* (aviso de seguridad privado).
- **Alternativa:** un correo a `<correo-de-seguridad>` (a definir por quien
  mantiene el proyecto antes de la primera versión pública).

Incluí, si podés:

- la versión de Liviano (Ajustes → Actualizaciones) y de Windows;
- qué pasa y qué esperabas que pasara;
- los pasos para reproducirlo, o una prueba de concepto;
- el impacto que le ves (qué podría hacer quien lo aproveche).

## Qué pasa después

1. Respondemos dentro de los **3 días hábiles** para confirmar que lo recibimos.
2. Lo evaluamos y te contamos la severidad y el plan dentro de los **10 días**.
3. Publicamos la corrección por el actualizador firmado y, después, un aviso
   que te acredita (salvo que prefieras quedar anónimo).

Pedimos no divulgar el problema hasta que la corrección esté publicada, o
hasta 90 días desde el reporte, lo que ocurra primero.

## Alcance

Interesan especialmente:

- el actualizador: verificación de la firma Ed25519, reemplazo del ejecutable,
  descargas (`src/updater.rs`, `src/http.rs`);
- el instalador por usuario (`src/installer.rs`);
- la inyección de scripts en las páginas (modo lectura, ocultar elementos,
  video flotante);
- cualquier forma de que una página lea o cambie datos locales de Liviano.

Los fallos del motor WebView2 en sí se reportan a Microsoft
(<https://msrc.microsoft.com>); si afectan a Liviano de una forma particular,
avisanos también.

## Versiones con soporte

Sólo la última versión publicada recibe correcciones: el actualizador la
instala sola una vez por día.
