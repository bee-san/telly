/** Plays the audio track `listed` of those a video lists: a rip that plays as it is has all its tracks, and starts on its first. */
export function pickAudio(el: HTMLVideoElement, listed: number) {
  const tracks = (el as HTMLVideoElement & { audioTracks?: ArrayLike<{ enabled: boolean }> }).audioTracks;
  if (tracks?.[listed] && !tracks[listed].enabled) tracks[listed].enabled = true;
}
