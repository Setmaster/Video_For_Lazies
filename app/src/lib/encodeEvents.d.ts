export type EncodeEventUnlisten = () => void | Promise<void>;
export type EncodeEventSubscriber<T> = (
  handler: (payload: T) => void,
) => Promise<EncodeEventUnlisten>;

export type EncodeEventListenerOptions<TFinished, TProgress> = {
  subscribeFinished: EncodeEventSubscriber<TFinished>;
  subscribeProgress: EncodeEventSubscriber<TProgress>;
  onFinished: (payload: TFinished) => void;
  onProgress: (payload: TProgress) => void;
  onReady: () => void;
  onError: (error: unknown) => void;
};

export type EncodeEventListenerSubscription = {
  ready: Promise<boolean>;
  dispose: () => void;
};

export function installEncodeEventListeners<TFinished, TProgress>(
  options: EncodeEventListenerOptions<TFinished, TProgress>,
): EncodeEventListenerSubscription;
