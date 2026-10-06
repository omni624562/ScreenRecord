/** 以 `with { type: "text" }` 匯入的文字檔 */
declare module "*.md" {
  const text: string;
  export default text;
}
