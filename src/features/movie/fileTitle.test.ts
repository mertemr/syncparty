import { describe, expect, it } from "vitest";

import { titleFromFileName } from "./fileTitle";

describe("titleFromFileName", () => {
  it("cuts at the year", () => {
    expect(titleFromFileName("Stalker.1979.2160p.BluRay.x265.mkv")).toBe("Stalker");
  });

  it("keeps a multi-word title", () => {
    expect(titleFromFileName("No.Country.For.Old.Men.2007.1080p.WEB-DL.mp4")).toBe(
      "No Country For Old Men",
    );
  });

  it("cuts at a resolution when there is no year", () => {
    expect(titleFromFileName("Solaris 1080p BluRay.mkv")).toBe("Solaris");
  });

  it("handles brackets and underscores", () => {
    expect(titleFromFileName("[Group] Spirited_Away (2001) [BDRip].mkv")).toBe("Group Spirited Away");
  });

  it("takes the last segment of a path", () => {
    expect(titleFromFileName("D:\\Movies\\Heat.1995.mkv")).toBe("Heat");
  });

  it("leaves a plain name alone", () => {
    expect(titleFromFileName("Amelie.mkv")).toBe("Amelie");
  });

  it("keeps a year that is the whole name rather than returning nothing", () => {
    expect(titleFromFileName("1917.2019.1080p.mkv")).toBe("1917");
  });

  it("does not split web-dl into a boundary of its own", () => {
    expect(titleFromFileName("Dune Part Two 2024 WEB-DL.mkv")).toBe("Dune Part Two");
  });
});
