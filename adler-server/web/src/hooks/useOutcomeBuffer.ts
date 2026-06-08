import { actions } from "../store";

export function useOutcomeBuffer(): (outcome: unknown) => void {
    let outcomeBuffer: unknown[] = [];
    let outcomeRafQueued = false;

    return (outcome: unknown) => {
        outcomeBuffer.push(outcome);
        if (outcomeRafQueued) return;
        outcomeRafQueued = true;
        requestAnimationFrame(() => {
            const batch = outcomeBuffer as Parameters<typeof actions.appendOutcomes>[0];
            outcomeBuffer = [];
            outcomeRafQueued = false;
            actions.appendOutcomes(batch);
        });
    };
}
