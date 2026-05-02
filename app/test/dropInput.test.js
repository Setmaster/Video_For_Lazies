import test from "node:test";
import assert from "node:assert/strict";

import {
  DEFAULT_UNSUPPORTED_DROP_MESSAGE,
  bindWindowFileDrop,
  extractDroppedPathsFromDataTransfer,
  inferDroppedFormat,
  isFileDragTypeList,
  parseDroppedUriList,
  pickDroppedVideoPath,
  resolveDroppedVideoAction,
  resolveDroppedVideo,
} from "../src/lib/dropInput.mjs";

function createFakeDropTarget() {
  const listeners = new Map();

  return {
    addEventListener(type, listener) {
      const current = listeners.get(type) ?? [];
      current.push(listener);
      listeners.set(type, current);
    },
    removeEventListener(type, listener) {
      const current = listeners.get(type) ?? [];
      listeners.set(
        type,
        current.filter((candidate) => candidate !== listener),
      );
    },
    dispatch(type, event) {
      for (const listener of listeners.get(type) ?? []) {
        listener(event);
      }
    },
  };
}

function createDragEvent({ types = ["Files"], files = [], relatedTarget = null, getData } = {}) {
  let prevented = 0;
  return {
    dataTransfer: {
      types,
      files,
      getData: getData ?? (() => ""),
    },
    relatedTarget,
    preventDefault() {
      prevented += 1;
    },
    get prevented() {
      return prevented;
    },
  };
}

test("pickDroppedVideoPath returns the first supported video path", () => {
  assert.equal(
    pickDroppedVideoPath([
      "C:\\\\Users\\\\Example\\\\Documents\\\\notes.txt",
      "C:\\\\Users\\\\Example\\\\Videos\\\\clip.MOV",
      "C:\\\\Users\\\\Example\\\\Videos\\\\clip.mp4",
    ]),
    "C:\\\\Users\\\\Example\\\\Videos\\\\clip.MOV",
  );
});

test("inferDroppedFormat only switches to mp4 or webm automatically", () => {
  assert.equal(inferDroppedFormat("C:\\\\Videos\\\\clip.mp4", "webm"), "mp4");
  assert.equal(inferDroppedFormat("C:\\\\Videos\\\\clip.webm", "mp4"), "webm");
  assert.equal(inferDroppedFormat("C:\\\\Videos\\\\clip.mov", "mp3"), "mp3");
});

test("resolveDroppedVideo returns the selected path and next format", () => {
  assert.deepEqual(
    resolveDroppedVideo(
      ["C:\\\\Users\\\\Example\\\\Documents\\\\notes.txt", "C:\\\\Users\\\\Example\\\\Videos\\\\clip.webm"],
      "mp4",
    ),
    {
      path: "C:\\\\Users\\\\Example\\\\Videos\\\\clip.webm",
      nextFormat: "webm",
    },
  );
});

test("isFileDragTypeList detects file drags", () => {
  assert.equal(isFileDragTypeList(["text/plain"]), false);
  assert.equal(isFileDragTypeList(["Files", "text/plain"]), true);
});

test("resolveDroppedVideoAction applies the first supported dropped video", () => {
  assert.deepEqual(
    resolveDroppedVideoAction({
      paths: ["C:\\\\Users\\\\Example\\\\Documents\\\\notes.txt", "C:\\\\Users\\\\Example\\\\Videos\\\\clip.webm"],
      currentFormat: "mp4",
      jobId: null,
    }),
    {
      kind: "applyInput",
      clearDragActive: true,
      path: "C:\\\\Users\\\\Example\\\\Videos\\\\clip.webm",
      nextFormat: "webm",
    },
  );
});

test("resolveDroppedVideoAction reports unsupported drops", () => {
  assert.deepEqual(
    resolveDroppedVideoAction({
      paths: ["C:\\\\Users\\\\Example\\\\Documents\\\\notes.txt"],
      currentFormat: "mp4",
      jobId: null,
    }),
    {
      kind: "status",
      clearDragActive: true,
      message: DEFAULT_UNSUPPORTED_DROP_MESSAGE,
    },
  );
});

test("resolveDroppedVideoAction ignores drops while a job is running", () => {
  assert.deepEqual(
    resolveDroppedVideoAction({
      paths: ["C:\\\\Users\\\\Example\\\\Videos\\\\clip.mp4"],
      currentFormat: "mp4",
      jobId: 42,
    }),
    {
      kind: "ignore",
      clearDragActive: true,
    },
  );
});

test("parseDroppedUriList handles Windows and POSIX file URIs", () => {
  assert.deepEqual(
    parseDroppedUriList("file:///C:/Example/Videos/clip%20one.mp4\nfile:///tmp/example/Videos/clip-two.webm"),
    [
      "C:/Example/Videos/clip one.mp4",
      "/tmp/example/Videos/clip-two.webm",
    ],
  );
});

test("extractDroppedPathsFromDataTransfer uses file.path when available", () => {
  const dataTransfer = {
    files: [
      { path: "C:\\\\Users\\\\Example\\\\Videos\\\\clip-one.mp4" },
      { path: "C:\\\\Users\\\\Example\\\\Videos\\\\clip-two.webm" },
    ],
    getData() {
      return "";
    },
  };

  assert.deepEqual(extractDroppedPathsFromDataTransfer(dataTransfer), [
    "C:\\\\Users\\\\Example\\\\Videos\\\\clip-one.mp4",
    "C:\\\\Users\\\\Example\\\\Videos\\\\clip-two.webm",
  ]);
});

test("extractDroppedPathsFromDataTransfer falls back to file URIs and de-dupes", () => {
  const dataTransfer = {
    files: [{ path: "C:/Example/Videos/clip-one.mp4" }],
    getData(type) {
      if (type === "text/uri-list") {
        return "file:///C:/Example/Videos/clip-one.mp4\nfile:///C:/Example/Videos/clip-two.webm";
      }
      return "";
    },
  };

  assert.deepEqual(extractDroppedPathsFromDataTransfer(dataTransfer), [
    "C:/Example/Videos/clip-one.mp4",
    "C:/Example/Videos/clip-two.webm",
  ]);
});

test("bindWindowFileDrop activates the overlay and forwards dropped paths", () => {
  const target = createFakeDropTarget();
  const dragStates = [];
  const droppedPaths = [];

  const cleanup = bindWindowFileDrop(target, {
    isDropAllowed: () => true,
    onDragActiveChange(active) {
      dragStates.push(active);
    },
    onPathsDropped(paths) {
      droppedPaths.push(paths);
    },
  });

  const enterEvent = createDragEvent();
  target.dispatch("dragenter", enterEvent);

  const dropEvent = createDragEvent({
    files: [{ path: "C:\\\\Users\\\\Example\\\\Videos\\\\clip-one.mp4" }],
  });
  target.dispatch("drop", dropEvent);
  cleanup();

  assert.equal(enterEvent.prevented, 1);
  assert.equal(dropEvent.prevented, 1);
  assert.deepEqual(dragStates, [true, false]);
  assert.deepEqual(droppedPaths, [["C:\\\\Users\\\\Example\\\\Videos\\\\clip-one.mp4"]]);
});

test("bindWindowFileDrop ignores drops while input changes are blocked", () => {
  const target = createFakeDropTarget();
  const dragStates = [];
  const droppedPaths = [];

  const cleanup = bindWindowFileDrop(target, {
    isDropAllowed: () => false,
    onDragActiveChange(active) {
      dragStates.push(active);
    },
    onPathsDropped(paths) {
      droppedPaths.push(paths);
    },
  });

  target.dispatch("dragenter", createDragEvent());
  target.dispatch(
    "drop",
    createDragEvent({
      files: [{ path: "C:\\\\Users\\\\Example\\\\Videos\\\\clip-one.mp4" }],
    }),
  );
  cleanup();

  assert.deepEqual(dragStates, [false]);
  assert.deepEqual(droppedPaths, []);
});
