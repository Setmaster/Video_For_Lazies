export type EncodeEventUnlisten = () => void | Promise<void>;
export type EncodeEventSubscriber<T> = (
  handler: (payload: T) => void,
) => Promise<EncodeEventUnlisten>;

export const ENCODE_EVENT_REGISTRATION_TIMEOUT_MS: number;

export type EncodeEventScheduler = {
  setTimeout: (callback: () => void, delayMs: number) => unknown;
  clearTimeout: (handle: unknown) => void;
};

export type EncodeEventListenerOptions<TFinished, TProgress> = {
  subscribeFinished: EncodeEventSubscriber<TFinished>;
  subscribeProgress: EncodeEventSubscriber<TProgress>;
  onFinished: (payload: TFinished) => void;
  onProgress: (payload: TProgress) => void;
  onReady: () => void;
  onError: (error: unknown) => void;
  registrationTimeoutMs?: number;
  scheduler?: EncodeEventScheduler;
};

export type EncodeEventListenerSubscription = {
  ready: Promise<boolean>;
  dispose: () => void;
};

export function installEncodeEventListeners<TFinished, TProgress>(
  options: EncodeEventListenerOptions<TFinished, TProgress>,
): EncodeEventListenerSubscription;
