import test from "node:test";
import assert from "node:assert/strict";

import {
  DEFAULT_QUEUE_FULL_DROP_MESSAGE,
  DEFAULT_UNSUPPORTED_DROP_MESSAGE,
  bindWindowFileDrop,
  classifyDroppedVideoPaths,
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
      unsupportedCount: 1,
      duplicateCount: 0,
      invalidCount: 0,
      overflowCount: 0,
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
      unsupportedCount: 1,
      duplicateCount: 0,
      invalidCount: 0,
      overflowCount: 0,
      message: DEFAULT_UNSUPPORTED_DROP_MESSAGE,
    },
  );
});

test("resolveDroppedVideoAction queues a drop while an export is busy", () => {
  assert.deepEqual(
    resolveDroppedVideoAction({
      paths: ["C:\\\\Users\\\\Example\\\\Videos\\\\clip.mp4"],
      currentFormat: "mp4",
      jobId: 42,
    }),
    {
      kind: "queueInputs",
      clearDragActive: true,
      unsupportedCount: 0,
      duplicateCount: 0,
      invalidCount: 0,
      overflowCount: 0,
      paths: ["C:\\\\Users\\\\Example\\\\Videos\\\\clip.mp4"],
      format: "mp4",
      reason: "busy",
    },
  );
});

test("classifyDroppedVideoPaths preserves order and reports unsupported and duplicate paths", () => {
  assert.deepEqual(
    classifyDroppedVideoPaths([
      "C:\\\\Videos\\\\First.MP4",
      "c:/videos/first.mp4",
      "C:\\\\Videos\\\\notes.txt",
      "C:\\\\Videos\\\\Second.webm",
      null,
    ], { platform: "windows" }),
    {
      supportedPaths: ["C:\\\\Videos\\\\First.MP4", "C:\\\\Videos\\\\Second.webm"],
      unsupportedPaths: ["C:\\\\Videos\\\\notes.txt"],
      unsupportedCount: 2,
      duplicateCount: 1,
      invalidCount: 1,
    },
  );
});

test("idle multi-file drops queue accepted videos in first-occurrence order", () => {
  assert.deepEqual(
    resolveDroppedVideoAction({
      paths: [
        "C:\\\\Videos\\\\First.MP4",
        "c:/videos/first.mp4",
        "C:\\\\Videos\\\\notes.txt",
        "C:\\\\Videos\\\\Second.webm",
      ],
      currentFormat: "mp3",
      jobId: null,
      queueCapacity: 10,
      platform: "windows",
    }),
    {
      kind: "queueInputs",
      clearDragActive: true,
      unsupportedCount: 1,
      duplicateCount: 1,
      invalidCount: 0,
      overflowCount: 0,
      paths: ["C:\\\\Videos\\\\First.MP4", "C:\\\\Videos\\\\Second.webm"],
      format: "mp3",
      reason: "multiple",
    },
  );
});

test("multi-file drop partially accepts the first paths that fit", () => {
  assert.deepEqual(
    resolveDroppedVideoAction({
      paths: ["/videos/one.mp4", "/videos/two.webm", "/videos/three.mov"],
      currentFormat: "mp4",
      queueCapacity: 2,
      platform: "posix",
    }),
    {
      kind: "queueInputs",
      clearDragActive: true,
      unsupportedCount: 0,
      duplicateCount: 0,
      invalidCount: 0,
      overflowCount: 1,
      paths: ["/videos/one.mp4", "/videos/two.webm"],
      format: "mp4",
      reason: "multiple",
    },
  );
});

test("busy drops report a full queue instead of disappearing", () => {
  assert.deepEqual(
    resolveDroppedVideoAction({
      paths: ["/videos/one.mp4"],
      currentFormat: "mp4",
      busy: true,
      queueCapacity: 0,
      platform: "posix",
    }),
    {
      kind: "status",
      clearDragActive: true,
      unsupportedCount: 0,
      duplicateCount: 0,
      invalidCount: 0,
      overflowCount: 1,
      message: DEFAULT_QUEUE_FULL_DROP_MESSAGE,
    },
  );
});

test("empty drops remain an explicit no-op", () => {
  assert.deepEqual(
    resolveDroppedVideoAction({ paths: [], currentFormat: "mp4" }),
    {
      kind: "ignore",
      clearDragActive: true,
      unsupportedCount: 0,
      duplicateCount: 0,
      invalidCount: 0,
      overflowCount: 0,
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

test("extractDroppedPathsFromDataTransfer de-dupes Windows slash and case variants", () => {
  const dataTransfer = {
    files: [{ path: "C:\\\\Example\\\\Videos\\\\Clip-One.mp4" }],
    getData(type) {
      if (type === "text/uri-list") {
        return "file:///c:/example/videos/clip-one.mp4\nfile:///C:/Example/Videos/clip-two.webm";
      }
      return "";
    },
  };

  assert.deepEqual(extractDroppedPathsFromDataTransfer(dataTransfer), [
    "C:\\\\Example\\\\Videos\\\\Clip-One.mp4",
    "C:/Example/Videos/clip-two.webm",
  ]);
});

test("extractDroppedPathsFromDataTransfer de-dupes file URI and native UNC spellings", () => {
  const dataTransfer = {
    files: [{ path: "\\\\Server\\Share\\Clip.mp4" }],
    getData(type) {
      if (type === "text/uri-list") {
        return "file://server/share/clip.mp4\nfile://server/share/second.webm";
      }
      return "";
    },
  };

  assert.deepEqual(extractDroppedPathsFromDataTransfer(dataTransfer), [
    "\\\\Server\\Share\\Clip.mp4",
    "//server/share/second.webm",
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
