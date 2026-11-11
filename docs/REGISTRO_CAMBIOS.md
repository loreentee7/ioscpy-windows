# Registro de cambios y contexto

Este documento recoge lo que solicita el propietario, los cambios realizados y su estado, para poder continuar el trabajo desde distintos ordenadores. Las comprobaciones pendientes no deben interpretarse como pruebas superadas.

## 11 de noviembre de 2026 — Conexión por Wi-Fi

**Petición:** controlar el iPhone con jailbreak desde el PC a través de Wi-Fi, además de la conexión USB existente.

**Cambios realizados:** se ha añadido la opción `--wifi USER@HOST` al cliente, que inicia un túnel SSH hasta el puerto local del servicio ioscpy en el iPhone. El modo USB sigue siendo el predeterminado. El README incluye un ejemplo y los requisitos. Se eligió SSH porque el servicio del iPhone escucha en loopback y la conexión de red de `iproxy` no puede acceder a ese puerto directamente.

**Requisitos:** servidor SSH activo en el iPhone, cliente OpenSSH en el PC, conectividad entre ambos y credenciales SSH. No guardar credenciales en este repositorio.

**Estado y pruebas:** cambio de código preparado; no compilado ni probado con el iPhone. Se intentó `cargo test`, pero falta `link.exe` (herramientas C++ de Visual Studio) en el entorno. No se ha generado un ejecutable. Antes de dar la función por terminada, instalar o configurar las herramientas de compilación y probar conexión, imagen, control y reconexión en un dispositivo real. `git diff --check` no detectó errores de espacios.

## 11 de noviembre de 2026 — Registro compartido y Git

**Petición:** documentar cada solicitud y actuación para dar contexto a la IA en ambos ordenadores, y subir a Git cada cambio, incluido este documento.

**Cambios realizados:** creado este registro y `AGENTS.md` con las instrucciones para consultarlo y actualizarlo. El remoto de trabajo identificado es `origin` (`loreentee7/ioscpy-windows`); `upstream` corresponde al proyecto de origen.

## 11 de noviembre de 2026 — Preparación de la prueba Wi-Fi

**Información aportada:** el propietario confirma que OpenSSH está instalado en el iPhone. La dirección local se facilita durante la prueba; puede cambiar, por lo que no se fija en el código.

**Actuación:** se añadió un flujo de GitHub Actions para ejecutar pruebas y compilar el ejecutable de Windows, ya que la instalación local de Visual Studio está incompleta. La primera comprobación TCP del puerto SSH no respondió desde este PC; instalación de OpenSSH no confirma que el servicio esté activo o que haya conectividad. Queda pendiente verificar la ejecución del flujo y realizar la prueba real con el iPhone.
