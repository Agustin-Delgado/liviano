// Floating video: the engine's own Picture-in-Picture window, which stays
// above other apps. Pressing again brings the video back. Evaluated through
// the DevTools protocol with a user gesture: from a plain script the engine
// refuses ("Must be handling a user gesture").
(async () => {
  /*TOAST*/
  if (document.pictureInPictureElement) {
    await document.exitPictureInPicture();
    return "exit";
  }
  if (!document.pictureInPictureEnabled) {
    livianoToast("Este motor no permite sacar videos a una ventana flotante");
    return "unsupported";
  }

  const videos = [...document.querySelectorAll("video")];
  // Same-origin frames only: a cross-origin frame's document is not ours to read.
  for (const frame of document.querySelectorAll("iframe")) {
    try {
      if (frame.contentDocument) videos.push(...frame.contentDocument.querySelectorAll("video"));
    } catch (error) {
      // Cross-origin.
    }
  }

  // The visible area, so a thumbnail or an off-screen preview loses to the
  // main player; a playing video beats a paused one of any size.
  const score = (video) => {
    const r = video.getBoundingClientRect();
    const view = video.ownerDocument.defaultView;
    const width = Math.max(0, Math.min(r.right, view.innerWidth) - Math.max(r.left, 0));
    const height = Math.max(0, Math.min(r.bottom, view.innerHeight) - Math.max(r.top, 0));
    const playing = !video.paused && !video.ended && video.readyState > 2;
    return (playing ? 1e9 : 0) + width * height;
  };
  const candidates = videos.filter((v) => v.readyState > 0 && score(v) > 0);
  if (!candidates.length) {
    livianoToast("No hay ningún video a la vista en esta página");
    return "none";
  }
  candidates.sort((a, b) => score(b) - score(a));
  const video = candidates[0];
  // A site may opt its player out; the person asked for it explicitly.
  video.disablePictureInPicture = false;
  try {
    await video.requestPictureInPicture();
    return "enter";
  } catch (error) {
    livianoToast("No se pudo sacar el video a una ventana flotante");
    return "error: " + error.name + ": " + error.message;
  }
})()
