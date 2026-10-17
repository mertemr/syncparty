import { VIDEO_EXTENSIONS } from "@/features/movie/fileTitle";

export type FileFilter = { name: string; extensions: string[] };

/**
 * The filters on the watch card's "Choose your copy…" dialog.
 *
 * Two quirks of the platform dialogs shape this. On Linux the dialog is
 * GTK 3's, which matches `*.mkv` case-sensitively, so `Movie.MKV` or a
 * phone's `VID_0001.MP4` would be hidden by a lowercase-only list; hence
 * the uppercase twin of every extension. Mixed case (`.Mkv`) and formats mpv
 * plays that are not on the list still need a way in, so an "All files"
 * filter follows. The backend checks that whatever comes back is a regular
 * file, so offering everything here weakens nothing.
 *
 * The video filter goes first: GTK and Windows select the first filter, so
 * the dialog opens on movies, not on the whole folder.
 */
export function videoFileFilters(names: {
  videos: string;
  allFiles: string;
}): FileFilter[] {
  return [
    {
      name: names.videos,
      extensions: VIDEO_EXTENSIONS.flatMap((extension) => [
        extension,
        extension.toUpperCase(),
      ]),
    },
    // rfd turns "*" into `*.*` on GTK and Windows and a bare `*` on the
    // desktop portal: everything with an extension, which every video has.
    { name: names.allFiles, extensions: ["*"] },
  ];
}
