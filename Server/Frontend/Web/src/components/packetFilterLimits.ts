/**
 * 定义封包滤镜搜索行与替换行的统一字节容量。
 * 前端网格、提交校验与后端编译器必须保持 512 字节一致，避免界面可编辑但保存被拒绝。
 */
export const maximumPacketFilterBytes = 512;
