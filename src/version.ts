/** 版本資訊：版本號以 package.json 為唯一來源，更新說明為 CHANGELOG.md（建置時一併打包進 exe）。 */
import pkg from "../package.json";
import changelog from "../CHANGELOG.md" with { type: "text" };

export const APP_VERSION: string = pkg.version;
export const CHANGELOG: string = changelog;
