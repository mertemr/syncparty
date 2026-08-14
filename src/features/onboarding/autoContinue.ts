/**
 * Whether the setup screen should hand straight over without being seen.
 *
 * The setting does not mean "never check" — the check still runs, and a
 * machine that has lost a dependency still gets the screen, with the reason on
 * it. It means "do not interrupt me when there is nothing to say".
 */
export function shouldAutoContinue({
  enabled,
  satisfied,
  alreadyUsed,
}: {
  enabled: boolean;
  satisfied: boolean;
  alreadyUsed: boolean;
}): boolean {
  return enabled && satisfied && !alreadyUsed;
}
