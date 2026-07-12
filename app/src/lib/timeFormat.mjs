export function formatClock(totalSeconds) {
  if (!Number.isFinite(totalSeconds) || totalSeconds < 0) return "0:00";

  const totalCentiseconds = Math.round(totalSeconds * 100);
  if (!Number.isSafeInteger(totalCentiseconds) || totalCentiseconds < 0) return "0:00";

  const hours = Math.floor(totalCentiseconds / 360_000);
  const centisecondsWithinHour = totalCentiseconds % 360_000;
  const minutes = Math.floor(centisecondsWithinHour / 6_000);
  const centisecondsWithinMinute = centisecondsWithinHour % 6_000;
  const seconds = Math.floor(centisecondsWithinMinute / 100);
  const centiseconds = centisecondsWithinMinute % 100;
  const secondsText = `${String(seconds).padStart(2, "0")}.${String(centiseconds).padStart(2, "0")}`;

  if (hours > 0) {
    return `${hours}:${String(minutes).padStart(2, "0")}:${secondsText}`;
  }

  return `${minutes}:${secondsText}`;
}
