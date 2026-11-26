#pragma once

#include <hyprland/src/render/Framebuffer.hpp>
#include <hyprland/src/render/pass/PassElement.hpp>

namespace PicaLens {

enum class Filter { Linear, Fsr, Sgsr };

class Upscaler {
  public:
    explicit Upscaler(Filter filter);
    ~Upscaler();

    bool prepare(const Vector2D& monitor, const Vector2D& source, const CBox& destination);
    void begin();
    bool finish(const CRegion& damage);

  private:
    struct Program {
        GLuint id = 0;
        GLint sampler = -1;
        GLint size = -1;

        bool compile(const std::string& algorithm);
        void draw(CFramebuffer& destination, const SP<CTexture>& texture) const;
    };

    bool initialize();

    Filter m_filter;
    Program m_upscale;
    Program m_sharpen;
    GLuint m_vao = 0;
    GLint m_maxTexture = 0;
    CFramebuffer m_capture;
    CFramebuffer m_source;
    CFramebuffer m_intermediate;
    CFramebuffer m_output;
    CFramebuffer* m_previous = nullptr;
    CBox m_destination;
};

}
