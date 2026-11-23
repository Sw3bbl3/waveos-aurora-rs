// vkcheck: Vulkan on Mesa's software driver (lavapipe), checked by reading
// the picture back. It draws a red triangle over a dark blue background
// into a 64x64 image with a graphics pipeline (vertex buffer, vertex and
// fragment shaders compiled by lavapipe's LLVM JIT, a render pass), copies
// the image to a buffer and checks pixels inside and outside the triangle.
//
// Built by xtask (xtask/src/mesa.rs) with clang against Debian's
// libvulkan-dev headers, for the Mesa test disk (`cargo xtask test --mesa`).
// The shaders are SPIR-V written out by hand below (no shader compiler is
// needed to build it); their GLSL is in the comments.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <vulkan/vulkan.h>

#define W 64
#define H 64

#define CHECK(x)                                                        \
    do {                                                                \
        VkResult r_ = (x);                                              \
        if (r_ != VK_SUCCESS) {                                         \
            fprintf(stderr, "vkcheck: %s failed: %d\n", #x, (int)r_);   \
            return 1;                                                   \
        }                                                               \
    } while (0)

// A SPIR-V instruction's first word: word count and opcode.
#define OP(words, opcode) (((uint32_t)(words) << 16) | (uint32_t)(opcode))
#define MAIN 0x6E69616D, 0 // "main"

// #version 450
// layout(location = 0) in vec4 pos;
// void main() { gl_Position = pos; }
static const uint32_t vert[] = {
    0x07230203, 0x00010000, 0, 17, 0,
    OP(2, 17), 1,                    // OpCapability Shader
    OP(3, 14), 0, 1,                 // OpMemoryModel Logical GLSL450
    OP(7, 15), 0, 1, MAIN, 2, 3,     // OpEntryPoint Vertex %1 "main" %2 %3
    OP(4, 71), 2, 30, 0,             // OpDecorate %2 Location 0
    OP(5, 72), 9, 0, 11, 0,          // OpMemberDecorate %9 0 BuiltIn Position
    OP(3, 71), 9, 2,                 // OpDecorate %9 Block
    OP(2, 19), 4,                    // %4 = OpTypeVoid
    OP(3, 33), 5, 4,                 // %5 = OpTypeFunction %4
    OP(3, 22), 6, 32,                // %6 = OpTypeFloat 32
    OP(4, 23), 7, 6, 4,              // %7 = OpTypeVector %6 4
    OP(4, 32), 8, 1, 7,              // %8 = OpTypePointer Input %7
    OP(4, 59), 8, 2, 1,              // %2 = OpVariable %8 Input
    OP(3, 30), 9, 7,                 // %9 = OpTypeStruct %7 (gl_PerVertex)
    OP(4, 32), 10, 3, 9,             // %10 = OpTypePointer Output %9
    OP(4, 59), 10, 3, 3,             // %3 = OpVariable %10 Output
    OP(4, 21), 11, 32, 1,            // %11 = OpTypeInt 32 1
    OP(4, 43), 11, 12, 0,            // %12 = OpConstant %11 0
    OP(4, 32), 13, 3, 7,             // %13 = OpTypePointer Output %7
    OP(5, 54), 4, 1, 0, 5,           // %1 = OpFunction %4 None %5
    OP(2, 248), 14,                  // OpLabel
    OP(4, 61), 7, 15, 2,             // %15 = OpLoad %7 %2
    OP(5, 65), 13, 16, 3, 12,        // %16 = OpAccessChain %13 %3 %12
    OP(3, 62), 16, 15,               // OpStore %16 %15
    OP(1, 253),                      // OpReturn
    OP(1, 56),                       // OpFunctionEnd
};

// #version 450
// layout(location = 0) out vec4 color;
// void main() { color = vec4(1.0, 0.0, 0.0, 1.0); }
static const uint32_t frag[] = {
    0x07230203, 0x00010000, 0, 12, 0,
    OP(2, 17), 1,                    // OpCapability Shader
    OP(3, 14), 0, 1,                 // OpMemoryModel Logical GLSL450
    OP(6, 15), 4, 1, MAIN, 2,        // OpEntryPoint Fragment %1 "main" %2
    OP(3, 16), 1, 7,                 // OpExecutionMode %1 OriginUpperLeft
    OP(4, 71), 2, 30, 0,             // OpDecorate %2 Location 0
    OP(2, 19), 3,                    // %3 = OpTypeVoid
    OP(3, 33), 4, 3,                 // %4 = OpTypeFunction %3
    OP(3, 22), 5, 32,                // %5 = OpTypeFloat 32
    OP(4, 23), 6, 5, 4,              // %6 = OpTypeVector %5 4
    OP(4, 32), 7, 3, 6,              // %7 = OpTypePointer Output %6
    OP(4, 59), 7, 2, 3,              // %2 = OpVariable %7 Output
    OP(4, 43), 5, 8, 0x3f800000,     // %8 = OpConstant %5 1.0
    OP(4, 43), 5, 9, 0,              // %9 = OpConstant %5 0.0
    OP(7, 44), 6, 10, 8, 9, 9, 8,    // %10 = OpConstantComposite %6 %8 %9 %9 %8
    OP(5, 54), 3, 1, 0, 4,           // %1 = OpFunction %3 None %4
    OP(2, 248), 11,                  // OpLabel
    OP(3, 62), 2, 10,                // OpStore %2 %10
    OP(1, 253),                      // OpReturn
    OP(1, 56),                       // OpFunctionEnd
};

static VkPhysicalDeviceMemoryProperties mem_props;

// A memory type allowed by `bits` with the properties `want`.
static int memory_type(uint32_t bits, VkMemoryPropertyFlags want) {
    for (uint32_t i = 0; i < mem_props.memoryTypeCount; i++)
        if ((bits & (1u << i)) && (mem_props.memoryTypes[i].propertyFlags & want) == want)
            return (int)i;
    return -1;
}

static VkResult bind_memory(VkDevice dev, VkMemoryRequirements req, VkMemoryPropertyFlags want,
                            VkDeviceMemory *mem) {
    int type = memory_type(req.memoryTypeBits, want);
    if (type < 0)
        return VK_ERROR_OUT_OF_DEVICE_MEMORY;
    VkMemoryAllocateInfo ai = {VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, NULL, req.size, (uint32_t)type};
    return vkAllocateMemory(dev, &ai, NULL, mem);
}

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static int near(const uint8_t *px, int r, int g, int b, int a) {
    int want[4] = {r, g, b, a};
    for (int i = 0; i < 4; i++)
        if (abs(px[i] - want[i]) > 2)
            return 0;
    return 1;
}

int main(void) {
    double start = now();
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                             .pApplicationName = "vkcheck",
                             .apiVersion = VK_API_VERSION_1_1};
    VkInstanceCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app};
    VkInstance inst;
    CHECK(vkCreateInstance(&ici, NULL, &inst));

    VkPhysicalDevice pds[8];
    uint32_t count = 8;
    VkResult r = vkEnumeratePhysicalDevices(inst, &count, pds);
    if (r != VK_SUCCESS && r != VK_INCOMPLETE) {
        fprintf(stderr, "vkcheck: no physical devices (%d)\n", (int)r);
        return 1;
    }
    VkPhysicalDevice pd = VK_NULL_HANDLE;
    VkPhysicalDeviceProperties props;
    for (uint32_t i = 0; i < count; i++) {
        vkGetPhysicalDeviceProperties(pds[i], &props);
        printf("vkcheck: device %u: %s (Vulkan %u.%u.%u)\n", i, props.deviceName, VK_API_VERSION_MAJOR(props.apiVersion),
               VK_API_VERSION_MINOR(props.apiVersion), VK_API_VERSION_PATCH(props.apiVersion));
        if (pd == VK_NULL_HANDLE && strstr(props.deviceName, "llvmpipe"))
            pd = pds[i];
    }
    if (pd == VK_NULL_HANDLE) {
        fprintf(stderr, "vkcheck: lavapipe (llvmpipe) is not among the %u devices\n", count);
        return 1;
    }
    vkGetPhysicalDeviceProperties(pd, &props);
    vkGetPhysicalDeviceMemoryProperties(pd, &mem_props);

    VkQueueFamilyProperties qf[8];
    uint32_t nqf = 8;
    vkGetPhysicalDeviceQueueFamilyProperties(pd, &nqf, qf);
    uint32_t family = UINT32_MAX;
    for (uint32_t i = 0; i < nqf; i++)
        if (qf[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) {
            family = i;
            break;
        }
    if (family == UINT32_MAX) {
        fprintf(stderr, "vkcheck: no graphics queue\n");
        return 1;
    }
    float priority = 1.0f;
    VkDeviceQueueCreateInfo qci = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
                                   .queueFamilyIndex = family,
                                   .queueCount = 1,
                                   .pQueuePriorities = &priority};
    VkDeviceCreateInfo dci = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, .queueCreateInfoCount = 1,
                              .pQueueCreateInfos = &qci};
    VkDevice dev;
    CHECK(vkCreateDevice(pd, &dci, NULL, &dev));
    VkQueue queue;
    vkGetDeviceQueue(dev, family, 0, &queue);

    // The picture: an image to render into.
    VkImageCreateInfo imci = {.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
                              .imageType = VK_IMAGE_TYPE_2D,
                              .format = VK_FORMAT_R8G8B8A8_UNORM,
                              .extent = {W, H, 1},
                              .mipLevels = 1,
                              .arrayLayers = 1,
                              .samples = VK_SAMPLE_COUNT_1_BIT,
                              .tiling = VK_IMAGE_TILING_OPTIMAL,
                              .usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT,
                              .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED};
    VkImage image;
    CHECK(vkCreateImage(dev, &imci, NULL, &image));
    VkMemoryRequirements req;
    vkGetImageMemoryRequirements(dev, image, &req);
    VkDeviceMemory image_mem;
    CHECK(bind_memory(dev, req, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT, &image_mem));
    CHECK(vkBindImageMemory(dev, image, image_mem, 0));
    VkImageViewCreateInfo ivci = {.sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
                                  .image = image,
                                  .viewType = VK_IMAGE_VIEW_TYPE_2D,
                                  .format = VK_FORMAT_R8G8B8A8_UNORM,
                                  .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1}};
    VkImageView view;
    CHECK(vkCreateImageView(dev, &ivci, NULL, &view));

    // Host-visible buffers: the triangle's corners and the picture read back.
    const VkMemoryPropertyFlags host = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
    VkBuffer buffers[2];
    VkDeviceMemory buffer_mem[2];
    VkDeviceSize sizes[2] = {3 * 4 * sizeof(float), W * H * 4};
    VkBufferUsageFlags usages[2] = {VK_BUFFER_USAGE_VERTEX_BUFFER_BIT, VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    for (int i = 0; i < 2; i++) {
        VkBufferCreateInfo bci = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = sizes[i], .usage = usages[i]};
        CHECK(vkCreateBuffer(dev, &bci, NULL, &buffers[i]));
        vkGetBufferMemoryRequirements(dev, buffers[i], &req);
        CHECK(bind_memory(dev, req, host, &buffer_mem[i]));
        CHECK(vkBindBufferMemory(dev, buffers[i], buffer_mem[i], 0));
    }
    // Apex at the top middle, base near the bottom (Vulkan's y points down).
    const float corners[12] = {0.0f, -0.8f, 0.0f, 1.0f, 0.8f, 0.8f, 0.0f, 1.0f, -0.8f, 0.8f, 0.0f, 1.0f};
    void *mapped;
    CHECK(vkMapMemory(dev, buffer_mem[0], 0, sizes[0], 0, &mapped));
    memcpy(mapped, corners, sizeof corners);
    vkUnmapMemory(dev, buffer_mem[0]);

    VkAttachmentDescription att = {.format = VK_FORMAT_R8G8B8A8_UNORM,
                                   .samples = VK_SAMPLE_COUNT_1_BIT,
                                   .loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR,
                                   .storeOp = VK_ATTACHMENT_STORE_OP_STORE,
                                   .stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE,
                                   .stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE,
                                   .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED,
                                   .finalLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL};
    VkAttachmentReference color_ref = {0, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
    VkSubpassDescription subpass = {.pipelineBindPoint = VK_PIPELINE_BIND_POINT_GRAPHICS,
                                    .colorAttachmentCount = 1,
                                    .pColorAttachments = &color_ref};
    VkRenderPassCreateInfo rpci = {.sType = VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO,
                                   .attachmentCount = 1,
                                   .pAttachments = &att,
                                   .subpassCount = 1,
                                   .pSubpasses = &subpass};
    VkRenderPass pass;
    CHECK(vkCreateRenderPass(dev, &rpci, NULL, &pass));
    VkFramebufferCreateInfo fbci = {.sType = VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,
                                    .renderPass = pass,
                                    .attachmentCount = 1,
                                    .pAttachments = &view,
                                    .width = W,
                                    .height = H,
                                    .layers = 1};
    VkFramebuffer fb;
    CHECK(vkCreateFramebuffer(dev, &fbci, NULL, &fb));

    VkShaderModule modules[2];
    const uint32_t *code[2] = {vert, frag};
    size_t code_size[2] = {sizeof vert, sizeof frag};
    for (int i = 0; i < 2; i++) {
        VkShaderModuleCreateInfo smci = {.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
                                         .codeSize = code_size[i],
                                         .pCode = code[i]};
        CHECK(vkCreateShaderModule(dev, &smci, NULL, &modules[i]));
    }
    VkPipelineShaderStageCreateInfo stages[2] = {
        {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
         .stage = VK_SHADER_STAGE_VERTEX_BIT,
         .module = modules[0],
         .pName = "main"},
        {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
         .stage = VK_SHADER_STAGE_FRAGMENT_BIT,
         .module = modules[1],
         .pName = "main"},
    };
    VkVertexInputBindingDescription binding = {0, 4 * sizeof(float), VK_VERTEX_INPUT_RATE_VERTEX};
    VkVertexInputAttributeDescription attr = {0, 0, VK_FORMAT_R32G32B32A32_SFLOAT, 0};
    VkPipelineVertexInputStateCreateInfo vi = {.sType = VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO,
                                               .vertexBindingDescriptionCount = 1,
                                               .pVertexBindingDescriptions = &binding,
                                               .vertexAttributeDescriptionCount = 1,
                                               .pVertexAttributeDescriptions = &attr};
    VkPipelineInputAssemblyStateCreateInfo ia = {.sType = VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
                                                 .topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST};
    VkViewport viewport = {0, 0, W, H, 0, 1};
    VkRect2D scissor = {{0, 0}, {W, H}};
    VkPipelineViewportStateCreateInfo vp = {.sType = VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
                                            .viewportCount = 1,
                                            .pViewports = &viewport,
                                            .scissorCount = 1,
                                            .pScissors = &scissor};
    VkPipelineRasterizationStateCreateInfo rs = {.sType = VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
                                                 .polygonMode = VK_POLYGON_MODE_FILL,
                                                 .cullMode = VK_CULL_MODE_NONE,
                                                 .frontFace = VK_FRONT_FACE_COUNTER_CLOCKWISE,
                                                 .lineWidth = 1.0f};
    VkPipelineMultisampleStateCreateInfo ms = {.sType = VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
                                               .rasterizationSamples = VK_SAMPLE_COUNT_1_BIT};
    VkPipelineColorBlendAttachmentState blend = {.colorWriteMask = VK_COLOR_COMPONENT_R_BIT | VK_COLOR_COMPONENT_G_BIT |
                                                                   VK_COLOR_COMPONENT_B_BIT | VK_COLOR_COMPONENT_A_BIT};
    VkPipelineColorBlendStateCreateInfo cb = {.sType = VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
                                              .attachmentCount = 1,
                                              .pAttachments = &blend};
    VkPipelineLayoutCreateInfo plci = {.sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO};
    VkPipelineLayout layout;
    CHECK(vkCreatePipelineLayout(dev, &plci, NULL, &layout));
    VkGraphicsPipelineCreateInfo gpci = {.sType = VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
                                         .stageCount = 2,
                                         .pStages = stages,
                                         .pVertexInputState = &vi,
                                         .pInputAssemblyState = &ia,
                                         .pViewportState = &vp,
                                         .pRasterizationState = &rs,
                                         .pMultisampleState = &ms,
                                         .pColorBlendState = &cb,
                                         .layout = layout,
                                         .renderPass = pass};
    VkPipeline pipeline;
    double compile = now();
    CHECK(vkCreateGraphicsPipelines(dev, VK_NULL_HANDLE, 1, &gpci, NULL, &pipeline));
    compile = now() - compile;

    VkCommandPoolCreateInfo cpci = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = family};
    VkCommandPool pool;
    CHECK(vkCreateCommandPool(dev, &cpci, NULL, &pool));
    VkCommandBufferAllocateInfo cbai = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                                        .commandPool = pool,
                                        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                                        .commandBufferCount = 1};
    VkCommandBuffer cmd;
    CHECK(vkAllocateCommandBuffers(dev, &cbai, &cmd));
    VkCommandBufferBeginInfo begin = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
                                      .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT};
    CHECK(vkBeginCommandBuffer(cmd, &begin));
    VkClearValue clear = {.color = {.float32 = {0.0f, 0.0f, 0.5f, 1.0f}}};
    VkRenderPassBeginInfo rpbi = {.sType = VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,
                                  .renderPass = pass,
                                  .framebuffer = fb,
                                  .renderArea = {{0, 0}, {W, H}},
                                  .clearValueCount = 1,
                                  .pClearValues = &clear};
    vkCmdBeginRenderPass(cmd, &rpbi, VK_SUBPASS_CONTENTS_INLINE);
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipeline);
    VkDeviceSize offset = 0;
    vkCmdBindVertexBuffers(cmd, 0, 1, &buffers[0], &offset);
    vkCmdDraw(cmd, 3, 1, 0, 0);
    vkCmdEndRenderPass(cmd);
    VkImageMemoryBarrier to_copy = {.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
                                    .srcAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT,
                                    .dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT,
                                    .oldLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                                    .newLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                                    .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
                                    .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
                                    .image = image,
                                    .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1}};
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL,
                         0, NULL, 1, &to_copy);
    VkBufferImageCopy region = {.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1}, .imageExtent = {W, H, 1}};
    vkCmdCopyImageToBuffer(cmd, image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buffers[1], 1, &region);
    VkBufferMemoryBarrier to_host = {.sType = VK_STRUCTURE_TYPE_BUFFER_MEMORY_BARRIER,
                                     .srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
                                     .dstAccessMask = VK_ACCESS_HOST_READ_BIT,
                                     .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
                                     .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
                                     .buffer = buffers[1],
                                     .size = VK_WHOLE_SIZE};
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_HOST_BIT, 0, 0, NULL, 1, &to_host, 0,
                         NULL);
    CHECK(vkEndCommandBuffer(cmd));

    VkFenceCreateInfo fci = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence;
    CHECK(vkCreateFence(dev, &fci, NULL, &fence));
    VkSubmitInfo si = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd};
    double draw = now();
    CHECK(vkQueueSubmit(queue, 1, &si, fence));
    CHECK(vkWaitForFences(dev, 1, &fence, VK_TRUE, 10ull * 1000 * 1000 * 1000));
    draw = now() - draw;

    CHECK(vkMapMemory(dev, buffer_mem[1], 0, sizes[1], 0, &mapped));
    const uint8_t *px = mapped;
    const uint8_t *inside = px + ((H / 2) * W + W / 2) * 4;
    const uint8_t *outside = px + (2 * W + 2) * 4;
    printf("vkcheck: %s drew a triangle: inside rgba(%d, %d, %d, %d), outside rgba(%d, %d, %d, %d)\n",
           props.deviceName, inside[0], inside[1], inside[2], inside[3], outside[0], outside[1], outside[2],
           outside[3]);
    printf("vkcheck: pipeline compiled in %.0f ms, drawn and read back in %.0f ms, %.0f ms in all\n", compile * 1000,
           draw * 1000, (now() - start) * 1000);
    int ok = near(inside, 255, 0, 0, 255) && (near(outside, 0, 0, 128, 255) || near(outside, 0, 0, 127, 255));
    vkUnmapMemory(dev, buffer_mem[1]);

    vkDestroyFence(dev, fence, NULL);
    vkDestroyCommandPool(dev, pool, NULL);
    vkDestroyPipeline(dev, pipeline, NULL);
    vkDestroyPipelineLayout(dev, layout, NULL);
    for (int i = 0; i < 2; i++) {
        vkDestroyShaderModule(dev, modules[i], NULL);
        vkDestroyBuffer(dev, buffers[i], NULL);
        vkFreeMemory(dev, buffer_mem[i], NULL);
    }
    vkDestroyFramebuffer(dev, fb, NULL);
    vkDestroyRenderPass(dev, pass, NULL);
    vkDestroyImageView(dev, view, NULL);
    vkDestroyImage(dev, image, NULL);
    vkFreeMemory(dev, image_mem, NULL);
    vkDestroyDevice(dev, NULL);
    vkDestroyInstance(inst, NULL);
    if (!ok) {
        fprintf(stderr, "vkcheck: the picture is wrong\n");
        return 1;
    }
    printf("vkcheck passed\n");
    return 0;
}
