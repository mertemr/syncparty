import { useTranslate } from "@/shared/i18n";
import { Button } from "@/shared/ui";
import type { ParticipationStatus } from "@/shared/types/ParticipationStatus";

/** "Going / Maybe / Not going" — set once, changeable any time the vote is
 * open. Shared between the host's own entry and every guest's. */
export function ParticipationPicker({
  value,
  onChange,
  disabled,
}: {
  value: ParticipationStatus | null;
  onChange: (status: ParticipationStatus) => void;
  disabled?: boolean;
}) {
  const t = useTranslate();

  const options: Array<{ status: ParticipationStatus; label: string }> = [
    { status: "going", label: t("movieVote.going") },
    { status: "maybe", label: t("movieVote.maybe") },
    { status: "notGoing", label: t("movieVote.notGoing") },
  ];

  return (
    <div className="flex gap-2">
      {options.map((option) => (
        <Button
          key={option.status}
          variant={value === option.status ? "primary" : "secondary"}
          className="flex-1"
          disabled={disabled}
          onClick={() => onChange(option.status)}
        >
          {option.label}
        </Button>
      ))}
    </div>
  );
}
