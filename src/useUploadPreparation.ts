import { useCallback, useEffect, useRef, useState } from "react";
import { analyzeUploadSelection } from "./bridge";
import type {
  UploadAdvisory,
  UploadAdvisoryInput,
  UploadPreparationDecision,
} from "./types";

export function useUploadPreparation(autoArchive: boolean) {
  const [uploadAdvisory, setUploadAdvisory] = useState<UploadAdvisory | null>(null);
  const decisionResolverRef = useRef<((decision: UploadPreparationDecision) => void) | null>(null);

  const chooseUploadPreparation = useCallback((decision: UploadPreparationDecision) => {
    const resolve = decisionResolverRef.current;
    decisionResolverRef.current = null;
    setUploadAdvisory(null);
    resolve?.(decision);
  }, []);

  const requestUploadDecision = useCallback((advisory: UploadAdvisory): Promise<UploadPreparationDecision> => {
    decisionResolverRef.current?.("cancel");
    return new Promise((resolve) => {
      decisionResolverRef.current = resolve;
      setUploadAdvisory(advisory);
    });
  }, []);

  const resolveUploadPreparation = useCallback(async (
    items: UploadAdvisoryInput[],
  ): Promise<UploadPreparationDecision> => {
    if (!items.length) return "cancel";
    if (autoArchive) return "archive";

    const advisory = await analyzeUploadSelection(items);
    return advisory.shouldPrompt ? requestUploadDecision(advisory) : "direct";
  }, [autoArchive, requestUploadDecision]);

  useEffect(() => () => {
    const resolve = decisionResolverRef.current;
    decisionResolverRef.current = null;
    resolve?.("cancel");
  }, []);

  return {
    uploadAdvisory,
    resolveUploadPreparation,
    chooseUploadPreparation,
  };
}
