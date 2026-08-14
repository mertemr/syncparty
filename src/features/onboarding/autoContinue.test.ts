import { describe, expect, it } from "vitest";

import { shouldAutoContinue } from "./autoContinue";

describe("shouldAutoContinue", () => {
  it("shows the screen when the setting is off", () => {
    expect(
      shouldAutoContinue({
        enabled: false,
        satisfied: true,
        alreadyUsed: false,
      }),
    ).toBe(false);
  });

  it("skips the screen when everything is installed", () => {
    expect(
      shouldAutoContinue({ enabled: true, satisfied: true, alreadyUsed: false }),
    ).toBe(true);
  });

  /** The property the setting must not break: a missing player still stops. */
  it("shows the screen when something is missing, setting or not", () => {
    expect(
      shouldAutoContinue({
        enabled: true,
        satisfied: false,
        alreadyUsed: false,
      }),
    ).toBe(false);
  });

  /**
   * Stepping back has to land somewhere. Skipping again would throw the user
   * straight forward and make the screen unreachable while the setting is on.
   */
  it("stays put once the user has stepped back into it", () => {
    expect(
      shouldAutoContinue({ enabled: true, satisfied: true, alreadyUsed: true }),
    ).toBe(false);
  });
});
