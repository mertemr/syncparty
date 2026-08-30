/**
 * Guessing a movie title from the file everyone is watching.
 *
 * The room monitor reports a file name, which is the only thing syncparty
 * knows about what is actually on screen. Release names are a well-worn
 * convention — title, year, then a tail of technical tags — so the title is
 * everything before the first tag that could not be part of one.
 *
 * Deliberately not exact: this only seeds a search box the host confirms, so
 * a guess that is close enough to find the movie has done its whole job.
 */

/** The first of these ends the title. `\d{4}` covers the year, which is the
 * most reliable boundary of all; the rest catch names that carry no year. */
const TAIL = new RegExp(
  [
    "\\b(19|20)\\d{2}\\b",
    "\\b\\d{3,4}p\\b",
    "\\b(bluray|brrip|bdrip|webrip|web-?dl|hdtv|dvdrip|remux|hdrip|camrip)\\b",
    "\\b(x264|x265|h264|h265|hevc|avc|xvid|divx)\\b",
    "\\b(aac|ac3|dts|ddp?5|truehd|atmos)\\b",
    "\\b(multi|dual|dublaj|turkce|türkçe|altyazi|altyazı|subbed|dubbed)\\b",
    "\\b(extended|remastered|uncut|imax|proper|repack)\\b",
  ].join("|"),
  "i",
);

const EXTENSION = /\.(mkv|mp4|avi|m4v|mov|wmv|flv|webm|ts|m2ts|mpg|mpeg)$/i;

export function titleFromFileName(fileName: string): string {
  // A path, if the monitor reported one — only the last segment is the name.
  const base = fileName.split(/[\\/]/).pop() ?? fileName;

  const words = base
    .replace(EXTENSION, "")
    // Separators, in the three flavours release names use interchangeably.
    // Not spaces themselves, which are already separators.
    .replace(/[._]+/g, " ")
    // A hyphen is a separator between words but part of "web-dl", so it only
    // gives way when it has a space's job.
    .replace(/\s-\s|-{2,}/g, " ")
    .replace(/[[\](){}]/g, " ");

  // Searched from the second character on, so a title that is itself a year
  // — 1917, 2012 — is not mistaken for its own release tag and cut to
  // nothing. The boundary that matters is always the one after the title.
  const offset = words.slice(1).search(TAIL);
  const boundary = offset === -1 ? -1 : offset + 1;
  const title = (boundary > 0 ? words.slice(0, boundary) : words).trim();

  // Everything matched the tail — better to hand back the raw name than an
  // empty box the host has to work out for themselves.
  return title.replace(/\s{2,}/g, " ") || base.replace(EXTENSION, "");
}
