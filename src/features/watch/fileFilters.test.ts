import { describe, expect, it } from "vitest";

import { VIDEO_EXTENSIONS } from "@/features/movie/fileTitle";

import { videoFileFilters } from "./fileFilters";

const NAMES = { videos: "Video files", allFiles: "All files" };

/**
 * Whether a file name passes a filter the way GTK 3 applies it: each
 * extension becomes `*.<ext>`, matched case-sensitively.
 */
function gtkAccepts(filter: { extensions: string[] }, fileName: string) {
  return filter.extensions.some((extension) =>
    extension === "*"
      ? fileName.includes(".")
      : fileName.endsWith(`.${extension}`),
  );
}

describe("videoFileFilters", () => {
  it("opens on videos: the video filter comes first, under its own name", () => {
    const [videos] = videoFileFilters(NAMES);

    expect(videos.name).toBe("Video files");
    for (const extension of VIDEO_EXTENSIONS) {
      expect(videos.extensions).toContain(extension);
    }
  });

  it("lets an uppercase extension through a case-sensitive dialog", () => {
    const [videos] = videoFileFilters(NAMES);

    expect(gtkAccepts(videos, "Stalker.1979.mkv")).toBe(true);
    expect(gtkAccepts(videos, "Movie.MKV")).toBe(true);
    expect(gtkAccepts(videos, "VID_0001.MP4")).toBe(true);
    expect(gtkAccepts(videos, "notes.txt")).toBe(false);
  });

  it("offers every file as a second choice, for mixed case and formats off the list", () => {
    const filters = videoFileFilters(NAMES);

    expect(filters).toHaveLength(2);
    const all = filters[1];
    expect(all).toEqual({ name: "All files", extensions: ["*"] });
    expect(gtkAccepts(all, "b.Mkv")).toBe(true);
    expect(gtkAccepts(all, "home-video.3gp")).toBe(true);
  });
});
