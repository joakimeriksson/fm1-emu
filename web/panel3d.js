// The FM-1 in 3D (the "3D" toggle beside the screen controls): fm1.glb drawn with three.js, the
// firmware's screen on its LCD and its LEDs on the keys and buttons. Only a view: main.js keeps the
// machine's input; this module answers what is under a point (as the flat panel's panel_hit does)
// and draws what main.js hands it each frame. The wheel or a pinch on the empty panel zooms towards
// the point under it; dragging it tilts the synth, or when zoomed in moves it (Shift: the other);
// a double click shows the whole synth again.
// The model: tools/model3d (an original drawing, no product photo).
const THREE_URL = "https://cdn.jsdelivr.net/npm/three@0.170.0/+esm";
const GLTF_URL = "https://cdn.jsdelivr.net/npm/three@0.170.0/examples/jsm/loaders/GLTFLoader.js/+esm";

// what each part of the model is, in panel_hit's numbers: 0..6 an encoder (the flat panel's
// order), 7 the MASTER pot, 100 + matrix id a button or key
const ENCS = ["select", "presets", "algorithm", "knob1", "knob2", "knob3", "knob4"];
const BUTTONS = ["oct_down", "oct_up", "fx", "sel", "env", "lfo", "edit", "glo", "home", "save", "arp", "seq", "play_stop", "rec"];
const WHITES = [0, 2, 4, 6, 7, 9, 11, 12, 14, 16, 18, 19, 21, 23, 24, 26];   // F3..G5, left to right
const BLACKS = [1, 3, 5, 8, 10, 13, 15, 17, 20, 22, 25];
const AMBER = 0xf2a33a, GREEN = 0x62d36e;
const ASPECT = 1280 / 820;                 // the flat panel's, so the page's layout stays as it is
const CAM_KEY = "fm1.view3d.cam";

export async function panel3d(flat) {
  const [THREE, { GLTFLoader }] = await Promise.all([import(THREE_URL), import(GLTF_URL)]);
  const canvas = document.createElement("canvas");
  canvas.id = "panel3d";
  canvas.setAttribute("aria-label", "FM-1 front panel, in 3D");
  const renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: true });
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  const gltf = await new GLTFLoader().loadAsync(new URL("fm1.glb", import.meta.url).href);
  const root = gltf.scene;

  const scene = new THREE.Scene();
  scene.add(root);
  scene.add(new THREE.AmbientLight(0xffffff, 1.3));
  const sun = new THREE.DirectionalLight(0xffffff, 1.8);
  sun.position.set(-0.08, 0.25, 0.4);
  scene.add(sun);
  const camera = new THREE.PerspectiveCamera(28, ASPECT, 0.005, 5);

  // the parts, by what they are
  const parts = new Map();                 // node -> panel_hit number
  const byId = new Map();                  // panel_hit number -> { node, rest, mat, led }
  const own = (o) => { const m = o.material.clone(); o.material = m; return m; };
  const part = (name, id) => {
    const node = root.getObjectByName(name);
    if (!node) return;
    parts.set(node, id);
    byId.set(id, { node, rest: node.position.z, rot: node.rotation.z });
  };
  ENCS.forEach((n, i) => part(`knob_${n}`, i));
  part("knob_master", 7);
  BUTTONS.forEach((n, id) => {
    part(`button_${n}`, 100 + id);
    const p = byId.get(100 + id);
    if (p && p.node.material) { p.mat = own(p.node); p.tint = new THREE.Color(id === 12 ? GREEN : AMBER); }
  });
  const key = (name, id) => {
    part(name, 100 + id);
    const p = byId.get(100 + id);
    const led = root.getObjectByName(`${name}_led`);
    if (p && led?.material) {
      p.led = own(led);
      p.ledOff = p.led.color.clone();
      p.led.emissive = new THREE.Color(AMBER);
      p.led.emissiveIntensity = 0;
    }
  };
  WHITES.forEach((n, i) => key(`key_lower_${String(i).padStart(2, "0")}`, 14 + n));
  BLACKS.forEach((n, i) => key(`key_upper_${String(i).padStart(2, "0")}`, 14 + n));

  // the screen: 240 x 240, pixel for pixel
  const lcdCanvas = document.createElement("canvas");
  lcdCanvas.width = lcdCanvas.height = 240;
  const lcdCtx = lcdCanvas.getContext("2d");
  const lcdImg = lcdCtx.createImageData(240, 240);
  const lcdPx = new Uint32Array(lcdImg.data.buffer);
  const lcdTex = new THREE.CanvasTexture(lcdCanvas);
  Object.assign(lcdTex, { flipY: false, magFilter: THREE.NearestFilter, minFilter: THREE.LinearFilter, generateMipmaps: false, colorSpace: THREE.SRGBColorSpace });
  const lcd = root.getObjectByName("lcd");
  if (lcd) lcd.material = new THREE.MeshBasicMaterial({ map: lcdTex });
  const rgba = new Uint32Array(65536);
  for (let p = 0; p < 65536; p++) {
    const r = Math.round((p >> 11) * 255 / 31), g = Math.round(((p >> 5) & 63) * 255 / 63), b = Math.round((p & 31) * 255 / 31);
    rgba[p] = 0xff000000 | (b << 16) | (g << 8) | r;
  }

  // ------------------------------------------------------------ the camera: tilt, turn, zoom
  const box = new THREE.Box3().setFromObject(root);
  const size = box.getSize(new THREE.Vector3()), mid = box.getCenter(new THREE.Vector3());
  const fov = THREE.MathUtils.degToRad(camera.fov);
  // the distance at which the whole synth fits, with a little room
  const fitDist = () => 1.0 * Math.max(size.y / 2 / Math.tan(fov / 2), size.x / 2 / (Math.tan(fov / 2) * camera.aspect)) + size.z / 2;
  const home = () => ({ tilt: 0.26, turn: 0, zoom: 1, x: mid.x, y: mid.y });
  let cam = home();
  try { cam = { ...cam, ...JSON.parse(localStorage.getItem(CAM_KEY) || "{}") }; } catch { /* no storage: the default view */ }
  const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
  function place() {
    cam.tilt = clamp(cam.tilt, 0, 1.1);
    cam.turn = clamp(cam.turn, -0.7, 0.7);
    cam.zoom = clamp(cam.zoom, 0.35, 1.2);
    cam.x = clamp(cam.x, box.min.x, box.max.x);
    cam.y = clamp(cam.y, box.min.y, box.max.y);
    const d = fitDist() * cam.zoom;
    camera.position.set(cam.x + d * Math.sin(cam.turn) * Math.cos(cam.tilt), cam.y - d * Math.sin(cam.tilt), d * Math.cos(cam.turn) * Math.cos(cam.tilt));
    camera.up.set(0, 1, 0);
    camera.lookAt(cam.x, cam.y, 0);
    dirty = true;
  }
  let saveTimer = 0;
  const keep = () => {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => { try { localStorage.setItem(CAM_KEY, JSON.stringify(cam)); } catch { /* not kept */ } }, 300);
  };

  // ------------------------------------------------------------ what is under a point
  const ray = new THREE.Raycaster();
  const ndc = (cx, cy) => {
    const r = canvas.getBoundingClientRect();
    return new THREE.Vector2(((cx - r.left) / r.width) * 2 - 1, -((cy - r.top) / r.height) * 2 + 1);
  };
  function hit(cx, cy) {
    ray.setFromCamera(ndc(cx, cy), camera);
    for (const h of ray.intersectObject(root, true)) {
      for (let o = h.object; o; o = o.parent) if (parts.has(o)) return parts.get(o);
      if (h.object.name.startsWith("enclosure")) return -1;
    }
    return -1;
  }
  // the point on the panel's face under a point of the canvas
  const plane = new THREE.Plane(new THREE.Vector3(0, 0, 1), 0);
  function onFace(cx, cy) {
    ray.setFromCamera(ndc(cx, cy), camera);
    return ray.ray.intersectPlane(plane, new THREE.Vector3());
  }
  function zoomAt(cx, cy, factor) {
    const p = onFace(cx, cy);
    const z0 = cam.zoom;
    cam.zoom = clamp(cam.zoom * factor, 0.35, 1.2);
    const f = 1 - cam.zoom / z0;            // towards the point as much as the distance shrank
    if (p) { cam.x += (p.x - cam.x) * f; cam.y += (p.y - cam.y) * f; }
    if (cam.zoom >= 1) { cam.x += (mid.x - cam.x) * 0.5; cam.y += (mid.y - cam.y) * 0.5; }
    place();
    keep();
  }

  // the empty panel (the enclosure, the screen): see the top
  const empty = new Map();                 // pointerId -> last [x, y]
  canvas.addEventListener("pointerdown", (e) => {
    if (hit(e.clientX, e.clientY) >= 0) return;
    empty.set(e.pointerId, [e.clientX, e.clientY]);
  });
  canvas.addEventListener("pointermove", (e) => {
    const last = empty.get(e.pointerId);
    if (!last) return;
    const [dx, dy] = [e.clientX - last[0], e.clientY - last[1]];
    if (empty.size === 2) {
      // a pinch: the change in the fingers' distance zooms, their middle moves
      const [a, b] = [...empty.entries()];
      const other = a[0] === e.pointerId ? b[1] : a[1];
      const before = Math.hypot(last[0] - other[0], last[1] - other[1]);
      const after = Math.hypot(e.clientX - other[0], e.clientY - other[1]);
      if (before > 10 && after > 10) zoomAt((e.clientX + other[0]) / 2, (e.clientY + other[1]) / 2, before / after);
    } else if ((cam.zoom < 0.95) !== e.shiftKey) {
      const span = fitDist() * cam.zoom * Math.tan(fov / 2) * 2 / canvas.getBoundingClientRect().height;
      cam.x -= dx * span;
      cam.y += dy * span;
      place();
    } else {
      cam.turn -= dx * 0.005;
      cam.tilt += dy * 0.005;
      place();
    }
    empty.set(e.pointerId, [e.clientX, e.clientY]);
    keep();
  });
  const up = (e) => empty.delete(e.pointerId);
  canvas.addEventListener("pointerup", up);
  canvas.addEventListener("pointercancel", up);
  canvas.addEventListener("wheel", (e) => {
    const h = hit(e.clientX, e.clientY);
    if (h >= 0 && h <= 7) return;                     // a knob: main.js turns it
    e.preventDefault();
    zoomAt(e.clientX, e.clientY, Math.exp(e.deltaY * 0.0015));
  }, { passive: false });
  canvas.addEventListener("dblclick", (e) => { if (hit(e.clientX, e.clientY) < 0) fit(); });
  function fit() { cam = home(); place(); keep(); }

  // ------------------------------------------------------------ size and drawing
  let dirty = true;
  function resize() {
    const w = Math.max(1, Math.round(canvas.clientWidth * devicePixelRatio));
    const h = Math.round(w / ASPECT);
    if (canvas.width !== w || canvas.height !== h) { renderer.setSize(w, h, false); camera.aspect = ASPECT; camera.updateProjectionMatrix(); dirty = true; }
  }
  const ro = new ResizeObserver(resize);
  canvas.width = 1280; canvas.height = 820;           // the layout's size before the first resize
  flat.after(canvas);
  flat.hidden = true;
  ro.observe(canvas);
  place();

  const amber = new THREE.Color(AMBER);
  const shown = new Map();                 // what was last drawn, to draw only on a change
  const same = (k, v) => shown.get(k) === v || (shown.set(k, v), false);
  return {
    canvas, hit,
    // the screen's pixels (RGB565), when they change
    lcd(px) {
      for (let i = 0; i < 57600; i++) lcdPx[i] = rgba[px[i]];
      lcdCtx.putImageData(lcdImg, 0, 0);
      lcdTex.needsUpdate = true;
      dirty = true;
    },
    // keys and buttons held, the LEDs (0..1 by matrix id), the encoders' clicks, the pot
    draw({ down, leds, encClicks, pot }) {
      for (const [id, p] of byId) {
        if (id === 7) {
          const a = (-135 + 270 * pot / 1023) * Math.PI / 180;
          if (!same(7, a)) { p.node.rotation.z = p.rot - a; dirty = true; }
        } else if (id < 7) {
          const a = encClicks[id] * 18 * Math.PI / 180;
          if (!same(id, a)) { p.node.rotation.z = p.rot - a; dirty = true; }
        } else {
          const m = id - 100, held = down[m], led = Math.min(1, Math.max(0, leds?.[m] || 0));
          if (same(id, held * 2 + led)) continue;
          p.node.position.z = p.rest - (held ? (p.led ? 0.0004 : 0.001) : 0);
          if (p.led) {
            const glow = held ? 1 : led;
            p.led.color.copy(p.ledOff).lerp(amber, glow);
            p.led.emissiveIntensity = glow * 1.6;
          }
          if (p.mat) { p.mat.emissive = p.tint; p.mat.emissiveIntensity = led * 0.6; }
          dirty = true;
        }
      }
      resize();
      if (dirty) { renderer.render(scene, camera); dirty = false; }
    },
    remove() {
      ro.disconnect();
      canvas.remove();
      flat.hidden = false;
      renderer.dispose();
    },
  };
}
