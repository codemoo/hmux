import { terminalFontFamily } from "./mobile.ts";

export function createTerminalFonts(isAndroid: boolean) {
  let androidFontFaces: Promise<void> | undefined;
  let androidReady = false;
  function loadAndroidFontFaces(): Promise<void> {
    if (androidFontFaces) return androidFontFaces;
    androidFontFaces = Promise.all(
      (
        [
          ["Regular", "400"],
          ["Bold", "700"],
        ] as const
      ).map(async ([name, weight]) => {
        const face = new FontFace(
          "HMux Android Mono",
          `url(/fonts/MonatendardNFM-${name}.woff2) format("woff2"), url(/fonts/MonatendardNFM-${name}.ttf) format("truetype")`,
          { weight, style: "normal", display: "block" },
        );
        await face.load();
        return face;
      }),
    )
      .then((faces) => {
        for (const face of faces) document.fonts.add(face);
        androidReady = true;
      })
      .catch((error) => {
        androidFontFaces = undefined;
        throw error;
      });
    return androidFontFaces;
  }
  const family = () =>
    isAndroid && androidReady
      ? '"HMux Android Mono", ' + terminalFontFamily
      : terminalFontFamily;

  async function load() {
    if (isAndroid) await loadAndroidFontFaces();
    const name = isAndroid ? "HMux Android Mono" : "HMux Mono";
    const faces = await Promise.all([
      document.fonts.load(`400 14px "${name}"`, "HMux 한글"),
      document.fonts.load(`700 14px "${name}"`, "HMux 한글"),
    ]);
    if (faces.some((loaded) => loaded.length === 0))
      throw new Error("Font unavailable");
  }
  return { family, load };
}
