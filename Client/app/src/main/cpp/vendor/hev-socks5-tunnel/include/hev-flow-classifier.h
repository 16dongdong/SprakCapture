#ifndef HEV_FLOW_CLASSIFIER_H
#define HEV_FLOW_CLASSIFIER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct HevFlowTuple HevFlowTuple;
typedef int (*HevFlowClassifier) (const HevFlowTuple *flow);

struct HevFlowTuple
{
    uint8_t protocol;
    uint8_t family;
    uint16_t source_port;
    uint16_t destination_port;
    uint8_t source_address[16];
    uint8_t destination_address[16];
};

/*
 * 登记会话五元组分类器和选中应用入口端口。
 * 端口字段使用主机序，地址字段保持网络字节序；返回 -1 的流不会建立 SOCKS 会话。
 */
void hev_socks5_tunnel_set_flow_classifier (HevFlowClassifier classifier,
                                             uint16_t selected_port);

#ifdef __cplusplus
}
#endif

#endif
