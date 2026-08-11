import { describe, expect, it } from "vitest";

import { createTransactionToolSeed } from "@/components/transactionToolSeed";
import { createTransactionSummary } from "#tests/testFixtures";

describe("事务工具上下文", () => {
  it("资源节点保留精确路径与查询参数", () => {
    const seed = createTransactionToolSeed(
      createTransactionSummary({
        transactionId: "resource-context",
        host: "api.example",
        port: 8443,
        path: "/v1/profile",
        query: "detail=full",
        urlDisplay: "https://api.example:8443/v1/profile?detail=full",
        contentType: "application/json",
      }),
    );

    expect(seed).toEqual({
      transactionId: "resource-context",
      contentType: "application/json",
      location: {
        protocol: "https",
        host: "api.example",
        port: "8443",
        path: "/v1/profile",
        query: "detail=full",
      },
    });
  });

  it("来源和目录节点不继承代表事务的查询参数", () => {
    const transaction = createTransactionSummary({
      host: "assets.example",
      path: "/images/logo.png",
      query: "cache=1",
      urlDisplay: "http://assets.example/images/logo.png?cache=1",
    });

    expect(createTransactionToolSeed(transaction, "").location).toMatchObject({
      protocol: "http",
      host: "assets.example",
      path: "",
      query: null,
    });
    expect(
      createTransactionToolSeed(transaction, "/images").location,
    ).toMatchObject({
      path: "/images",
      query: null,
    });
  });
});
